//! Capture narrative model: **Run → Story → Turn**, with **Call + Event** at the wire layer.
//!
//! | Layer | Type | Role |
//! |-------|------|------|
//! | Container | [`RunId`] / [`Run`] | One capture workspace |
//! | Narrative | [`StoryId`] / [`Story`] | One agent's full line |
//! | Dialogue | [`TurnId`] / [`Turn`] | One user→assistant round |
//! | Wire | [`Call`] + [`Event`] | HTTP call and what happened on it |

mod call;
mod context;
mod event;
mod ids;
mod model;
mod turn_machine;

pub use call::Call;
pub use context::{CallContext, StoryContext};
pub use event::{CancelEvent, CompleteEvent, DraftEvent, Event, RequestEvent};
pub use ids::{CallId, RunId, StoryId, TurnId};
pub use model::{
    EventKind, Run, Story, StoryLink, StoryLinkRelation, TextBlock, Turn, TurnCall, TurnKind,
};
pub use turn_machine::{TurnMachine, TurnObserveOutcome};
