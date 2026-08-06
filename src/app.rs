use crate::{
    cli::TtySizeLock,
    editor::Editor,
    event::AppEvent,
    focus::Focus,
    input_store::InputStore,
    keymap::{Action, normalized_key_event},
    pty::RunningProcess,
    template::{Configuration, EditorId, ExpansionError, ViewId},
    terminal::HostTerminal,
    ui::{self, Areas},
    view::{RunState, View},
};
use anyhow::{Result, anyhow};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use futures::StreamExt as _;
use mkutils::KeyMapSession;
use nix::libc;
use ratatui::layout::{Margin, Rect};
use std::{
    collections::{HashMap, HashSet},
    io::Read,
    time::{Duration, Instant},
};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
    time::MissedTickBehavior,
};

const FRAME_PERIOD: Duration = Duration::from_millis(16);
const RESTART_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Quit,
    Signal(i32),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DebugEvent {
    Key(KeyEvent),
    Mouse(MouseEvent),
}

pub struct App {
    input: InputStore,
    events: UnboundedReceiver<AppEvent>,
    event_sender: UnboundedSender<AppEvent>,
    pub(crate) views: Vec<View>,
    pub(crate) editors: Vec<Editor>,
    pub(crate) editor_order: Vec<EditorId>,
    affected_views: Vec<Vec<ViewId>>,
    pub(crate) focus: Focus,
    keymap: KeyMapSession<Action>,
    pending_restarts: HashMap<ViewId, Instant>,
    pub(crate) areas: Areas,
    pub(crate) debug_mode: bool,
    pub(crate) debug_event: Option<DebugEvent>,
}

impl App {
    pub fn new(
        configuration: Configuration,
        source: Box<dyn Read + Send>,
        tty_size_lock: Option<TtySizeLock>,
        keymap: KeyMapSession<Action>,
    ) -> Result<Self> {
        let (event_sender, events) = unbounded_channel();
        let input = InputStore::start(source, event_sender.clone())?;
        let focus = Focus::initial(configuration.views.len(), &configuration.editor_order);
        let views = configuration
            .views
            .into_iter()
            .map(|definition| View::new(definition, tty_size_lock))
            .collect();
        let editors = configuration
            .editors
            .into_iter()
            .map(|definition| Editor::new(definition.name, &definition.initial_text))
            .collect();
        Ok(Self {
            input,
            events,
            event_sender,
            views,
            editors,
            editor_order: configuration.editor_order,
            affected_views: configuration.affected_views,
            focus,
            keymap,
            pending_restarts: HashMap::new(),
            areas: Areas::default(),
            debug_mode: false,
            debug_event: None,
        })
    }

    pub async fn run(mut self) -> Result<RunOutcome> {
        let mut terminal = HostTerminal::new()?;
        let result = self.run_inner(&mut terminal).await;
        self.shutdown();
        let restore_result = terminal.restore();
        match (result, restore_result) {
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
            (Ok(outcome), Ok(())) => Ok(outcome),
        }
    }

    async fn run_inner(&mut self, terminal: &mut HostTerminal) -> Result<RunOutcome> {
        terminal.draw(|frame| ui::draw(frame, self))?;
        self.restart_views((0..self.views.len()).collect());

        let mut event_stream = EventStream::new();
        let mut frame_interval = tokio::time::interval(FRAME_PERIOD);
        frame_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        let mut hangup = signal(SignalKind::hangup())?;

        loop {
            tokio::select! {
                _ = frame_interval.tick() => {
                    if let Some(action) = self.keymap.on_tick().copied()
                        && let Some(outcome) = self.handle_action(action)
                    {
                        return Ok(outcome);
                    }
                    self.restart_due_views();
                    self.poll_children();
                    terminal.draw(|frame| ui::draw(frame, self))?;
                }
                event = event_stream.next() => {
                    let event = event.ok_or_else(|| anyhow!("terminal event stream ended"))??;
                    if let Some(outcome) = self.handle_terminal_event(event)? {
                        return Ok(outcome);
                    }
                }
                event = self.events.recv() => {
                    let event = event.ok_or_else(|| anyhow!("worker event channel ended"))?;
                    self.handle_worker_event(event)?;
                }
                _ = interrupt.recv() => return Ok(RunOutcome::Signal(libc::SIGINT)),
                _ = terminate.recv() => return Ok(RunOutcome::Signal(libc::SIGTERM)),
                _ = hangup.recv() => return Ok(RunOutcome::Signal(libc::SIGHUP)),
            }
        }
    }

    fn handle_terminal_event(&mut self, event: Event) -> Result<Option<RunOutcome>> {
        match event {
            Event::Key(key) => {
                self.debug_event = Some(DebugEvent::Key(key));
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                    self.handle_key(key)
                } else {
                    Ok(None)
                }
            }
            Event::Paste(pasted) => {
                self.handle_paste(&pasted)?;
                Ok(None)
            }
            Event::Mouse(mouse) => {
                self.debug_event = Some(DebugEvent::Mouse(mouse));
                self.handle_mouse(mouse)?;
                Ok(None)
            }
            Event::Resize(columns, rows) => {
                ui::resize(self, Rect::new(0, 0, columns, rows));
                Ok(None)
            }
            Event::FocusGained | Event::FocusLost => Ok(None),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Result<Option<RunOutcome>> {
        if let Some(action) = self.keymap.on_key_event(normalized_key_event(key)).copied() {
            if action == Action::ToggleDebug && key.kind == KeyEventKind::Repeat {
                return Ok(None);
            }
            if action == Action::FollowView && matches!(self.focus, Focus::Editor(_)) {
                // End remains an editor key when no view is focused.
            } else {
                return Ok(self.handle_action(action));
            }
        }

        match self.focus {
            Focus::Editor(editor) => {
                if self.editors[editor].input(key) {
                    self.schedule_editor_restart(editor, Instant::now());
                }
            }
            Focus::View(view) => {
                let application_cursor = self.views[view].terminal.application_cursor();
                if let Some(bytes) = encode_key(key, application_cursor)
                    && let Some(process) = &mut self.views[view].process
                {
                    process.write_input(&bytes)?;
                }
            }
        }
        Ok(None)
    }

    fn handle_paste(&mut self, pasted: &str) -> Result<()> {
        match self.focus {
            Focus::Editor(editor) => {
                if self.editors[editor].paste(pasted) {
                    self.schedule_editor_restart(editor, Instant::now());
                }
            }
            Focus::View(view) => {
                let bracketed = self.views[view].terminal.bracketed_paste();
                if let Some(process) = &mut self.views[view].process {
                    if bracketed {
                        process.write_input(b"\x1b[200~")?;
                    }
                    process.write_input(pasted.as_bytes())?;
                    if bracketed {
                        process.write_input(b"\x1b[201~")?;
                    }
                }
            }
        }
        Ok(())
    }

    fn handle_action(&mut self, action: Action) -> Option<RunOutcome> {
        match action {
            Action::FocusNext => self.focus.cycle(self.views.len(), &self.editor_order, true),
            Action::FocusPrevious => self.focus.cycle(self.views.len(), &self.editor_order, false),
            Action::Quit => return Some(RunOutcome::Quit),
            Action::LeaveView => self.leave_view(),
            Action::Restart => self.manual_restart(),
            Action::FollowView => {
                if let Focus::View(view) = self.focus {
                    self.views[view].terminal.follow();
                }
            }
            Action::ToggleDebug => self.debug_mode = !self.debug_mode,
        }
        None
    }

    fn leave_view(&mut self) {
        let Focus::View(view) = self.focus else {
            return;
        };
        let editor = self.views[view]
            .definition
            .referenced_editors
            .first()
            .copied()
            .or_else(|| self.editor_order.first().copied());
        if let Some(editor) = editor {
            self.focus = Focus::Editor(editor);
        }
    }

    fn manual_restart(&mut self) {
        let views = match self.focus {
            Focus::View(view) => vec![view],
            Focus::Editor(editor) => self.affected_views[editor].clone(),
        };
        for view in &views {
            self.pending_restarts.remove(view);
        }
        self.restart_views(views);
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        let position = ui::position(mouse.column, mouse.row);
        if matches!(mouse.kind, MouseEventKind::Moved) {
            if let Some((view, _)) = self
                .areas
                .views
                .iter()
                .enumerate()
                .find(|(_, area)| area.contains(position))
            {
                self.focus = Focus::View(view);
            }
            return Ok(());
        }

        if let Some(view) = self.areas.views.iter().position(|area| area.contains(position)) {
            let is_wheel = matches!(mouse.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown);
            if matches!(mouse.kind, MouseEventKind::Down(_)) || is_wheel {
                self.focus = Focus::View(view);
            }
            if self.forward_mouse_to_view(view, mouse)? {
                return Ok(());
            }
            if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                self.views[view].terminal.scroll_up(3);
            } else if matches!(mouse.kind, MouseEventKind::ScrollDown) {
                self.views[view].terminal.scroll_down(3);
            }
            return Ok(());
        }

        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && let Some(&(editor, area)) = self.areas.editors.iter().find(|(_, area)| area.contains(position))
        {
            self.focus = Focus::Editor(editor);
            let inner = area.inner(Margin::new(1, 1));
            if inner.contains(position) {
                self.editors[editor].place_cursor(mouse.column.saturating_sub(inner.x));
            }
        }
        Ok(())
    }

    fn forward_mouse_to_view(&mut self, view: ViewId, mouse: MouseEvent) -> Result<bool> {
        if !self.views[view].definition.use_pty {
            return Ok(false);
        }
        let inner = self.areas.views[view].inner(Margin::new(1, 1));
        let position = ui::position(mouse.column, mouse.row);
        if !inner.contains(position) {
            return Ok(false);
        }
        let row = mouse.row - inner.y;
        let column = mouse.column - inner.x;
        let terminal = &self.views[view].terminal;
        let (rows, columns) = terminal.size();
        if row >= rows || column >= columns {
            return Ok(false);
        }
        let Some(bytes) = encode_mouse(mouse, row, column, terminal) else {
            return Ok(false);
        };
        let Some(process) = &mut self.views[view].process else {
            return Ok(false);
        };
        process.write_input(&bytes)?;
        Ok(true)
    }

    fn handle_worker_event(&mut self, event: AppEvent) -> Result<()> {
        match event {
            AppEvent::InputAdvanced { len } => {
                let _ = len;
            }
            AppEvent::InputEof => {}
            AppEvent::PumpEnded { view, generation } => {
                let _is_current = self
                    .views
                    .get(view)
                    .is_some_and(|candidate| candidate.generation == generation);
            }
            AppEvent::InputFailed { error } | AppEvent::WorkerFailed { view: None, error, .. } => {
                return Err(anyhow!(error));
            }
            AppEvent::PtyOutput {
                view,
                generation,
                bytes,
            } => {
                if self
                    .views
                    .get(view)
                    .is_some_and(|candidate| candidate.generation == generation)
                {
                    let view = &mut self.views[view];
                    if view.definition.use_pty {
                        view.terminal.process(&bytes);
                    } else {
                        view.terminal.process_pipe_output(&bytes);
                    }
                }
            }
            AppEvent::WorkerFailed {
                view: Some(view),
                generation,
                error,
            } => {
                if generation.is_none_or(|generation| self.views[view].generation == generation) {
                    self.views[view].state = RunState::Error(error);
                }
            }
        }
        Ok(())
    }

    fn schedule_editor_restart(&mut self, editor: EditorId, now: Instant) {
        let deadline = now + RESTART_DEBOUNCE;
        for &view in &self.affected_views[editor] {
            self.pending_restarts.insert(view, deadline);
        }
    }

    fn restart_due_views(&mut self) {
        let now = Instant::now();
        let due = self
            .pending_restarts
            .iter()
            .filter_map(|(&view, &deadline)| (deadline <= now).then_some(view))
            .collect::<Vec<_>>();
        for view in &due {
            self.pending_restarts.remove(view);
        }
        if !due.is_empty() {
            self.restart_views(due);
        }
    }

    fn restart_views(&mut self, views: Vec<ViewId>) {
        self.validate_editors();
        let values = self
            .editors
            .iter()
            .map(|editor| editor.text.clone())
            .collect::<Vec<_>>();
        for view_id in views {
            if self.views[view_id]
                .definition
                .referenced_editors
                .iter()
                .any(|&editor| self.editors[editor].validation_error.is_some())
            {
                continue;
            }
            match self.views[view_id].definition.command.expand(&values) {
                Ok(arguments) => self.start_view(view_id, &arguments),
                Err(ExpansionError::InvalidEditor { editor, message }) => {
                    self.editors[editor].validation_error = Some(message);
                }
                Err(ExpansionError::EmptyCommand) => {
                    self.views[view_id].state = RunState::Error("command expands to an empty executable".to_owned());
                }
            }
        }
    }

    fn validate_editors(&mut self) {
        for editor in &mut self.editors {
            editor.validation_error = None;
        }
        let starred = self
            .views
            .iter()
            .flat_map(|view| view.definition.command.starred_editors())
            .collect::<HashSet<_>>();
        for editor in starred {
            if let Err(error) = shell_words::split(&self.editors[editor].text) {
                self.editors[editor].validation_error = Some(error.to_string());
            }
        }
    }

    fn start_view(&mut self, view_id: ViewId, arguments: &[String]) {
        let view = &mut self.views[view_id];
        let generation = view.next_generation();
        view.terminate();
        view.terminal.clear();
        view.state = RunState::Starting;
        let (rows, columns) = view.terminal.size();
        match RunningProcess::spawn(
            arguments,
            (rows, columns),
            &self.input,
            &self.event_sender,
            view_id,
            generation,
            view.definition.use_pty,
        ) {
            Ok(process) => {
                view.process = Some(process);
                view.state = RunState::Running;
            }
            Err(error) => {
                view.process = None;
                view.state = RunState::Error(error.to_string());
            }
        }
    }

    fn poll_children(&mut self) {
        for view in &mut self.views {
            view.poll_exit();
        }
    }

    fn shutdown(&mut self) {
        for view in &mut self.views {
            let _ = view.next_generation();
            view.terminate();
        }
    }
}

fn encode_key(event: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    if event.modifiers.contains(KeyModifiers::ALT) {
        bytes.push(0x1b);
    }
    match event.code {
        KeyCode::Char(character) if event.modifiers.contains(KeyModifiers::CONTROL) => {
            bytes.push(control_byte(character)?);
        }
        KeyCode::Char(character) => {
            let mut encoded = [0; 4];
            bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
        }
        KeyCode::Enter => bytes.push(b'\r'),
        KeyCode::Tab => bytes.push(b'\t'),
        KeyCode::Backspace => bytes.push(0x7f),
        KeyCode::Esc => bytes.push(0x1b),
        KeyCode::Null => bytes.push(0),
        KeyCode::Up => bytes.extend_from_slice(if application_cursor { b"\x1bOA" } else { b"\x1b[A" }),
        KeyCode::Down => bytes.extend_from_slice(if application_cursor { b"\x1bOB" } else { b"\x1b[B" }),
        KeyCode::Right => bytes.extend_from_slice(if application_cursor { b"\x1bOC" } else { b"\x1b[C" }),
        KeyCode::Left => bytes.extend_from_slice(if application_cursor { b"\x1bOD" } else { b"\x1b[D" }),
        KeyCode::Home => bytes.extend_from_slice(b"\x1b[H"),
        KeyCode::End => bytes.extend_from_slice(b"\x1b[F"),
        KeyCode::Insert => bytes.extend_from_slice(b"\x1b[2~"),
        KeyCode::Delete => bytes.extend_from_slice(b"\x1b[3~"),
        KeyCode::PageUp => bytes.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => bytes.extend_from_slice(b"\x1b[6~"),
        KeyCode::F(number) => bytes.extend_from_slice(function_key(number)?),
        KeyCode::BackTab => bytes.extend_from_slice(b"\x1b[Z"),
        KeyCode::CapsLock
        | KeyCode::ScrollLock
        | KeyCode::NumLock
        | KeyCode::PrintScreen
        | KeyCode::Pause
        | KeyCode::Menu
        | KeyCode::KeypadBegin
        | KeyCode::Media(_)
        | KeyCode::Modifier(_) => return None,
    }
    Some(bytes)
}

fn encode_mouse(
    mouse: MouseEvent,
    row: u16,
    column: u16,
    terminal: &crate::terminal_model::TerminalModel,
) -> Option<Vec<u8>> {
    let (mode, encoding) = terminal.mouse_protocol();
    if mode != vt100::MouseProtocolMode::None {
        let (button, release) = match mouse.kind {
            MouseEventKind::Down(button) => (mouse_button_code(button), false),
            MouseEventKind::Up(button) if mode != vt100::MouseProtocolMode::Press => (mouse_button_code(button), true),
            MouseEventKind::ScrollUp => (64, false),
            MouseEventKind::ScrollDown => (65, false),
            _ => return None,
        };
        return encode_mouse_event(button, release, row, column, mouse.modifiers, encoding);
    }
    match mouse.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if terminal.alternate_screen() => {
            let code = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                KeyCode::Up
            } else {
                KeyCode::Down
            };
            let key = encode_key(KeyEvent::new(code, KeyModifiers::NONE), terminal.application_cursor())?;
            Some(key.repeat(3))
        }
        _ => None,
    }
}

const fn mouse_button_code(button: MouseButton) -> u16 {
    match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

fn encode_mouse_event(
    mut button: u16,
    release: bool,
    row: u16,
    column: u16,
    modifiers: KeyModifiers,
    encoding: vt100::MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    button += u16::from(modifiers.contains(KeyModifiers::SHIFT)) * 4;
    button += u16::from(modifiers.contains(KeyModifiers::ALT)) * 8;
    button += u16::from(modifiers.contains(KeyModifiers::CONTROL)) * 16;
    let x = column.checked_add(1)?;
    let y = row.checked_add(1)?;

    if encoding == vt100::MouseProtocolEncoding::Sgr {
        let terminator = if release { 'm' } else { 'M' };
        return Some(format!("\x1b[<{button};{x};{y}{terminator}").into_bytes());
    }

    if release {
        button = 3
            + u16::from(modifiers.contains(KeyModifiers::SHIFT)) * 4
            + u16::from(modifiers.contains(KeyModifiers::ALT)) * 8
            + u16::from(modifiers.contains(KeyModifiers::CONTROL)) * 16;
    }

    let mut bytes = b"\x1b[M".to_vec();
    for value in [button, x, y] {
        let value = u32::from(value) + 32;
        match encoding {
            vt100::MouseProtocolEncoding::Default => bytes.push(u8::try_from(value).ok()?),
            vt100::MouseProtocolEncoding::Utf8 => {
                let character = char::from_u32(value).filter(|_| value <= 2_047)?;
                let mut encoded = [0; 4];
                bytes.extend_from_slice(character.encode_utf8(&mut encoded).as_bytes());
            }
            vt100::MouseProtocolEncoding::Sgr => unreachable!(),
        }
    }
    Some(bytes)
}

const fn control_byte(character: char) -> Option<u8> {
    match character {
        '@' | '`' | ' ' | '2' => Some(0),
        'a'..='z' => Some(character as u8 - b'a' + 1),
        'A'..='Z' => Some(character as u8 - b'A' + 1),
        '[' | '3' => Some(27),
        '\\' | '4' => Some(28),
        ']' | '5' => Some(29),
        '^' | '6' => Some(30),
        '_' | '7' => Some(31),
        '?' | '8' => Some(127),
        _ => None,
    }
}

const fn function_key(number: u8) -> Option<&'static [u8]> {
    match number {
        1 => Some(b"\x1bOP"),
        2 => Some(b"\x1bOQ"),
        3 => Some(b"\x1bOR"),
        4 => Some(b"\x1bOS"),
        5 => Some(b"\x1b[15~"),
        6 => Some(b"\x1b[17~"),
        7 => Some(b"\x1b[18~"),
        8 => Some(b"\x1b[19~"),
        9 => Some(b"\x1b[20~"),
        10 => Some(b"\x1b[21~"),
        11 => Some(b"\x1b[23~"),
        12 => Some(b"\x1b[24~"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{MediaKeyCode, ModifierKeyCode};

    fn new_app(configuration: Configuration, tty_size_lock: Option<TtySizeLock>) -> App {
        App::new(
            configuration,
            Box::new(std::io::Cursor::new(Vec::<u8>::new())),
            tty_size_lock,
            crate::keymap::load_keymap(None).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn debounce_is_trailing_edge_and_scoped_to_affected_views() {
        let configuration = Configuration::parse(&["one {shared}".to_owned(), "two {other}".to_owned()]).unwrap();
        let mut app = new_app(configuration, None);
        let first = Instant::now();
        app.schedule_editor_restart(0, first);
        let original = app.pending_restarts[&0];
        app.schedule_editor_restart(0, first + Duration::from_millis(50));
        assert!(app.pending_restarts[&0] > original);
        assert!(!app.pending_restarts.contains_key(&1));
    }

    #[test]
    fn initializes_prompt_editors_with_configured_text() {
        let configuration = Configuration::parse(&[r#"command "{query=hello world}""#.to_owned()]).unwrap();
        let app = new_app(configuration, None);
        assert_eq!(app.editors[0].text, "hello world");
        assert_eq!(app.editors[0].cursor, "hello world".len());
    }

    #[test]
    fn stale_generation_output_is_ignored() {
        let configuration = Configuration::parse(&["cat".to_owned()]).unwrap();
        let mut app = new_app(configuration, None);
        app.views[0].generation = 2;
        app.handle_worker_event(AppEvent::PtyOutput {
            view: 0,
            generation: 1,
            bytes: b"stale".to_vec(),
        })
        .unwrap();
        assert!(app.views[0].terminal.contents().is_empty());
        assert!(app.views[0].terminal.is_following());
    }

    #[test]
    fn terminal_resizes_propagate_to_views() {
        let configuration = Configuration::parse(&["one {value}".to_owned(), "two".to_owned()]).unwrap();
        let mut app = new_app(configuration, None);

        app.handle_terminal_event(Event::Resize(100, 30)).unwrap();

        assert_eq!(app.views[0].terminal.size(), (25, 48));
        assert_eq!(app.views[1].terminal.size(), (25, 48));
    }

    #[test]
    fn debug_mode_toggles_and_records_terminal_input_events() {
        let configuration = Configuration::parse(&["one".to_owned()]).unwrap();
        let mut app = new_app(configuration, None);
        let toggle = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL);

        app.handle_terminal_event(Event::Key(toggle)).unwrap();
        assert!(app.debug_mode);
        assert_eq!(app.debug_event, Some(DebugEvent::Key(toggle)));

        let mouse = MouseEvent {
            kind: MouseEventKind::Moved,
            column: 4,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        app.handle_terminal_event(Event::Mouse(mouse)).unwrap();
        assert_eq!(app.debug_event, Some(DebugEvent::Mouse(mouse)));

        app.handle_terminal_event(Event::Key(toggle)).unwrap();
        assert!(!app.debug_mode);
    }

    #[test]
    fn locked_tty_sizes_ignore_later_terminal_resizes() {
        let configuration = Configuration::parse(&["one".to_owned()]).unwrap();
        let mut initial = new_app(configuration, Some(TtySizeLock::Initial));
        initial.handle_terminal_event(Event::Resize(100, 30)).unwrap();
        initial.handle_terminal_event(Event::Resize(80, 20)).unwrap();
        assert_eq!(initial.views[0].terminal.size(), (28, 98));

        let configuration = Configuration::parse(&["one".to_owned()]).unwrap();
        let mut fixed = new_app(configuration, Some(TtySizeLock::Fixed { columns: 80, rows: 24 }));
        fixed.handle_terminal_event(Event::Resize(100, 30)).unwrap();
        assert_eq!(fixed.views[0].terminal.size(), (24, 80));

        let configuration = Configuration::parse(&["[no-tty] one".to_owned()]).unwrap();
        let mut piped = new_app(configuration, Some(TtySizeLock::Fixed { columns: 80, rows: 24 }));
        piped.handle_terminal_event(Event::Resize(100, 30)).unwrap();
        piped.handle_terminal_event(Event::Resize(80, 20)).unwrap();
        assert_eq!(piped.views[0].terminal.size(), (18, 78));
    }

    #[test]
    fn encodes_control_and_cursor_keys_for_the_pty() {
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), false).unwrap(),
            [3]
        );
        assert_eq!(
            encode_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true).unwrap(),
            b"\x1bOA"
        );
    }

    #[test]
    fn media_and_modifier_keys_are_not_encoded() {
        assert!(
            encode_key(
                KeyEvent::new(KeyCode::Media(MediaKeyCode::Play), KeyModifiers::NONE),
                false
            )
            .is_none()
        );
        assert!(
            encode_key(
                KeyEvent::new(KeyCode::Modifier(ModifierKeyCode::LeftShift), KeyModifiers::NONE),
                false
            )
            .is_none()
        );
    }

    #[test]
    fn encodes_wheel_for_child_mouse_protocols() {
        let mut terminal = crate::terminal_model::TerminalModel::new(400, 400);
        terminal.process(b"\x1b[?1000h");
        let default = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::SHIFT | KeyModifiers::CONTROL,
        };
        assert_eq!(encode_mouse(default, 0, 0, &terminal).unwrap(), b"\x1b[Mt!!");

        terminal.process(b"\x1b[?1005h");
        let utf8 = MouseEvent {
            modifiers: KeyModifiers::NONE,
            ..default
        };
        assert_eq!(encode_mouse(utf8, 0, 200, &terminal).unwrap(), b"\x1b[M`\xc3\xa9!");

        terminal.process(b"\x1b[?1006h");
        let sgr = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::ALT,
        };
        assert_eq!(encode_mouse(sgr, 2, 4, &terminal).unwrap(), b"\x1b[<73;5;3M");
    }

    #[test]
    fn encodes_clicks_for_requested_mouse_mode() {
        let mut terminal = crate::terminal_model::TerminalModel::new(10, 10);
        let left_down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::CONTROL,
        };
        assert!(encode_mouse(left_down, 2, 4, &terminal).is_none());

        terminal.process(b"\x1b[?9h");
        assert_eq!(encode_mouse(left_down, 2, 4, &terminal).unwrap(), b"\x1b[M0%#");
        let left_up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            ..left_down
        };
        assert!(encode_mouse(left_up, 2, 4, &terminal).is_none());

        terminal.process(b"\x1b[?1000h");
        let middle_down = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Middle),
            modifiers: KeyModifiers::ALT,
            ..left_down
        };
        let middle_up = MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Middle),
            ..middle_down
        };
        assert_eq!(encode_mouse(middle_up, 2, 4, &terminal).unwrap(), b"\x1b[M+%#");

        terminal.process(b"\x1b[?1006h");
        assert_eq!(encode_mouse(middle_down, 2, 4, &terminal).unwrap(), b"\x1b[<9;5;3M");
        assert_eq!(encode_mouse(middle_up, 2, 4, &terminal).unwrap(), b"\x1b[<9;5;3m");
    }

    #[test]
    fn clicks_still_focus_views_and_editors_without_tracking() {
        let configuration = Configuration::parse(&["one {value}".to_owned()]).unwrap();
        let mut app = new_app(configuration, None);
        app.handle_terminal_event(Event::Resize(30, 12)).unwrap();
        app.focus = Focus::Editor(0);
        let view_click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        };

        app.handle_terminal_event(Event::Mouse(view_click)).unwrap();

        assert_eq!(app.focus, Focus::View(0));
        let editor_area = app.areas.editors[0].1;
        let editor_click = MouseEvent {
            column: editor_area.x + 1,
            row: editor_area.y + 1,
            ..view_click
        };
        app.handle_terminal_event(Event::Mouse(editor_click)).unwrap();
        assert_eq!(app.focus, Focus::Editor(0));
    }

    #[test]
    fn alternate_screen_wheel_uses_application_cursor_keys() {
        let mut terminal = crate::terminal_model::TerminalModel::new(3, 10);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(encode_mouse(wheel, 0, 0, &terminal).is_none());

        terminal.process(b"\x1b[?1049h\x1b[?1h");

        assert_eq!(encode_mouse(wheel, 0, 0, &terminal).unwrap(), b"\x1bOB\x1bOB\x1bOB");
    }

    #[test]
    fn legacy_mouse_encodings_reject_unrepresentable_coordinates() {
        let mut terminal = crate::terminal_model::TerminalModel::new(3, 2_100);
        terminal.process(b"\x1b[?1000h");
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        assert!(encode_mouse(wheel, 0, 223, &terminal).is_none());

        terminal.process(b"\x1b[?1005h");

        assert!(encode_mouse(wheel, 0, 2_015, &terminal).is_none());
    }
}
