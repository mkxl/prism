use crate::template::ViewId;

#[derive(Debug)]
pub enum AppEvent {
    InputAdvanced {
        len: u64,
    },
    InputEof,
    InputFailed {
        error: String,
    },
    PtyOutput {
        view: ViewId,
        generation: u64,
        bytes: Vec<u8>,
    },
    PumpEnded {
        view: ViewId,
        generation: u64,
    },
    WorkerFailed {
        view: Option<ViewId>,
        generation: Option<u64>,
        error: String,
    },
}
