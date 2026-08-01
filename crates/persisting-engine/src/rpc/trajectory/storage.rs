//! RPC proto-to-domain storage selection mapping.

use persisting_pchronicle::StorageSelection;
use persisting_proto::TrajectoryStorageFormat;

pub(crate) fn to_selection(format: TrajectoryStorageFormat) -> StorageSelection {
    match format {
        TrajectoryStorageFormat::Auto => StorageSelection::Auto,
        TrajectoryStorageFormat::Lance => StorageSelection::Lance,
        TrajectoryStorageFormat::Markdown => StorageSelection::AgenticMd,
    }
}
