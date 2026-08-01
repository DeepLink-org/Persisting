//! Python bindings for the canonical Rust pChronicle implementation.

use persisting_pchronicle::{
    ingest_trajectory, reconstruct_trajectory, split_trajectory, AtifTrajectory,
    AtifTrajectoryView, Error, FsChronicleStore, MemoryChronicleStore, NormalizedStore, SessionRow,
    StepRow, ToolCallRow,
};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyAny;

enum Store {
    Memory(MemoryChronicleStore),
    Fs(FsChronicleStore),
}

impl Store {
    fn as_ref(&self) -> &dyn NormalizedStore {
        match self {
            Self::Memory(store) => store,
            Self::Fs(store) => store,
        }
    }

    fn as_mut(&mut self) -> &mut dyn NormalizedStore {
        match self {
            Self::Memory(store) => store,
            Self::Fs(store) => store,
        }
    }
}

/// Opaque owner of a Rust normalized pChronicle store.
#[pyclass(name = "_PChronicleStore", unsendable)]
pub(crate) struct PyChronicleStore {
    store: Store,
}

#[pymethods]
impl PyChronicleStore {
    #[new]
    #[pyo3(signature = (root=None))]
    fn new(root: Option<String>) -> PyResult<Self> {
        let store = match root {
            Some(root) => Store::Fs(FsChronicleStore::open(root).map_err(map_error)?),
            None => Store::Memory(MemoryChronicleStore::new()),
        };
        Ok(Self { store })
    }

    fn ingest(&mut self, trajectory: Bound<'_, PyAny>) -> PyResult<String> {
        let trajectory: AtifTrajectory = pythonize::depythonize(&trajectory)?;
        ingest_trajectory(self.store.as_mut(), &trajectory).map_err(map_error)
    }

    fn split(
        &self,
        py: Python<'_>,
        trajectory: Bound<'_, PyAny>,
    ) -> PyResult<(Py<PyAny>, Py<PyAny>, Py<PyAny>)> {
        let trajectory: AtifTrajectory = pythonize::depythonize(&trajectory)?;
        let split = split_trajectory(&trajectory).map_err(map_error)?;
        Ok((
            to_python(py, &split.session)?,
            to_python(py, &split.steps)?,
            to_python(py, &split.tool_calls)?,
        ))
    }

    fn reconstruct(&self, py: Python<'_>, session_id: &str) -> PyResult<Py<PyAny>> {
        let trajectory =
            reconstruct_trajectory(self.store.as_ref(), session_id).map_err(map_error)?;
        to_python(py, &trajectory)
    }

    #[pyo3(signature = (session_id=None))]
    fn query(&self, py: Python<'_>, session_id: Option<&str>) -> PyResult<Py<PyAny>> {
        let rows = AtifTrajectoryView::new(self.store.as_ref())
            .query(session_id)
            .map_err(map_error)?;
        to_python(py, &rows)
    }

    fn upsert_session(&mut self, row: Bound<'_, PyAny>) -> PyResult<()> {
        let row: SessionRow = pythonize::depythonize(&row)?;
        self.store.as_mut().upsert_session(row).map_err(map_error)
    }

    fn get_session(&self, py: Python<'_>, session_id: &str) -> PyResult<Py<PyAny>> {
        let row = self
            .store
            .as_ref()
            .get_session(session_id)
            .map_err(map_error)?;
        to_python(py, &row)
    }

    fn list_sessions(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        let rows = self.store.as_ref().list_sessions().map_err(map_error)?;
        to_python(py, &rows)
    }

    fn replace_steps(&mut self, session_id: &str, rows: Bound<'_, PyAny>) -> PyResult<()> {
        let rows: Vec<StepRow> = pythonize::depythonize(&rows)?;
        self.store
            .as_mut()
            .replace_steps(session_id, rows)
            .map_err(map_error)
    }

    fn list_steps(&self, py: Python<'_>, session_id: &str) -> PyResult<Py<PyAny>> {
        let rows = self
            .store
            .as_ref()
            .list_steps(session_id)
            .map_err(map_error)?;
        to_python(py, &rows)
    }

    fn replace_tool_calls(&mut self, session_id: &str, rows: Bound<'_, PyAny>) -> PyResult<()> {
        let rows: Vec<ToolCallRow> = pythonize::depythonize(&rows)?;
        self.store
            .as_mut()
            .replace_tool_calls(session_id, rows)
            .map_err(map_error)
    }

    fn list_tool_calls(&self, py: Python<'_>, session_id: &str) -> PyResult<Py<PyAny>> {
        let rows = self
            .store
            .as_ref()
            .list_tool_calls(session_id)
            .map_err(map_error)?;
        to_python(py, &rows)
    }
}

#[pyfunction]
pub(crate) fn pchronicle_atif_trajectory_sql_ddl() -> String {
    persisting_pchronicle::atif_trajectory_sql_ddl()
}

fn to_python<T: serde::Serialize>(py: Python<'_>, value: &T) -> PyResult<Py<PyAny>> {
    Ok(pythonize::pythonize(py, value)
        .map_err(|error| PyRuntimeError::new_err(error.to_string()))?
        .unbind())
}

fn map_error(error: Error) -> PyErr {
    let message = error.to_string();
    match error {
        Error::InvalidAtif(message) => PyValueError::new_err(message),
        Error::SessionNotFound(session_id) => PyKeyError::new_err(session_id),
        Error::DuplicateSession(_)
        | Error::DuplicateStep { .. }
        | Error::DuplicateToolCall { .. }
        | Error::OrphanToolCall { .. }
        | Error::Other(_) => PyValueError::new_err(message),
        Error::Io(_) | Error::Json(_) => PyRuntimeError::new_err(message),
    }
}
