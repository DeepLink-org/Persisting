//! Tiered Tensor Address Space (TTAS) — Rust implementation for Persisting.
//! See design: docs/src/design/tensor_address_algebra.md

mod block_io;
#[cfg(unix)]
mod mmap_region;
#[cfg(target_os = "macos")]
mod page_fault_darwin;
mod tiered_loop;
#[cfg(target_os = "linux")]
mod uffd;

use persisting_proto::{
    SearchAddBatchRequest, SearchAddRequest, SearchImportLanceRequest, SearchIndexDeleteRequest,
    SearchIndexListRequest, SearchIndexRebuildRequest, SearchIndexReorderRequest,
    SearchIndexRequest, SearchQueryRequest, TrajectoryAppendRequest, TrajectoryReplayRequest,
    TrajectoryStatsRequest, TrajectoryStorageFormat,
};
use pyo3::exceptions::{PyKeyError, PyRuntimeError, PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyBytes, PyDict, PyList};
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::RwLock;

// ---------------------------------------------------------------------------
// Dimension & CoordValue
// ---------------------------------------------------------------------------

#[pyclass(frozen)]
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Dimension {
    #[pyo3(get)]
    pub name: String,
    #[pyo3(get)]
    pub kind: String,
}

#[pymethods]
impl Dimension {
    #[new]
    fn new(name: String, kind: &str) -> PyResult<Self> {
        let k = match kind {
            "int" | "i64" => "int",
            "str" | "string" => "str",
            "bytes" => "bytes",
            _ => {
                return Err(PyValueError::new_err(format!(
                    "unknown dimension kind: {}",
                    kind
                )))
            }
        };
        Ok(Self {
            name,
            kind: k.to_string(),
        })
    }

    fn __repr__(&self) -> String {
        format!("Dimension({:?}, {})", self.name, self.kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum CoordValue {
    Int(i64),
    Str(String),
    Bytes(Vec<u8>),
}

impl CoordValue {
    fn as_int(&self) -> Option<i64> {
        match self {
            CoordValue::Int(x) => Some(*x),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Constraint: Point | Range | Set
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Constraint {
    Point(CoordValue),
    Range { lo: i64, hi: i64 },
    Set(BTreeSet<CoordValue>),
}

#[pyclass]
#[derive(Clone)]
pub struct Point {
    pub value: CoordValue,
}

#[pymethods]
impl Point {
    #[new]
    fn new(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        let v = coord_value_from_py(value)?;
        Ok(Self { value: v })
    }

    #[getter]
    fn value(&self, py: Python<'_>) -> PyObject {
        coord_value_to_py(&self.value, py)
    }
}

#[pyclass]
#[derive(Clone)]
pub struct Range {
    #[pyo3(get)]
    pub lo: i64,
    #[pyo3(get)]
    pub hi: i64,
}

#[pymethods]
impl Range {
    #[new]
    fn new(lo: i64, hi: i64) -> PyResult<Self> {
        if lo >= hi {
            return Err(PyValueError::new_err("Range requires lo < hi"));
        }
        Ok(Self { lo, hi })
    }
}

#[pyclass]
#[derive(Clone)]
pub struct SetC {
    pub values: BTreeSet<CoordValue>,
}

#[pymethods]
impl SetC {
    #[new]
    fn new(values: &Bound<'_, PyAny>) -> PyResult<Self> {
        let set = coord_set_from_py(values)?;
        if set.len() > 64 {
            return Err(PyValueError::new_err("SetC cardinality must be <= 64"));
        }
        Ok(Self { values: set })
    }

    #[getter]
    fn values(&self, py: Python<'_>) -> PyObject {
        let list = pyo3::types::PyList::empty(py);
        for v in &self.values {
            list.append(coord_value_to_py(v, py)).unwrap();
        }
        list.into_py(py)
    }
}

fn coord_value_from_py(obj: &Bound<'_, PyAny>) -> PyResult<CoordValue> {
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(CoordValue::Int(i));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(CoordValue::Str(s));
    }
    if let Ok(b) = obj.extract::<&[u8]>() {
        return Ok(CoordValue::Bytes(b.to_vec()));
    }
    if let Ok(b) = obj.extract::<Vec<u8>>() {
        return Ok(CoordValue::Bytes(b));
    }
    Err(PyValueError::new_err("value must be int, str, or bytes"))
}

fn coord_set_from_py(obj: &Bound<'_, PyAny>) -> PyResult<BTreeSet<CoordValue>> {
    let mut set = BTreeSet::new();
    for item in obj.try_iter()? {
        set.insert(coord_value_from_py(&item?)?);
    }
    Ok(set)
}

fn coord_value_to_py(value: &CoordValue, py: Python<'_>) -> PyObject {
    match value {
        CoordValue::Int(i) => i.into_pyobject(py).unwrap().into(),
        CoordValue::Str(s) => s.clone().into_pyobject(py).unwrap().into(),
        CoordValue::Bytes(b) => b.clone().into_pyobject(py).unwrap().into(),
    }
}

// ---------------------------------------------------------------------------
// Meet (constraint intersection)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MeetResult {
    Empty,
    Constraint(Constraint),
}

fn meet_constraint(c1: &Constraint, c2: &Constraint) -> MeetResult {
    use Constraint::*;
    match (c1, c2) {
        (Point(v1), Point(v2)) => {
            if v1 == v2 {
                MeetResult::Constraint(Point(v1.clone()))
            } else {
                MeetResult::Empty
            }
        }
        (Point(v), Range { lo, hi }) | (Range { lo, hi }, Point(v)) => {
            let i = match v.as_int() {
                Some(x) => x,
                None => return MeetResult::Empty,
            };
            if *lo <= i && i < *hi {
                MeetResult::Constraint(Point(v.clone()))
            } else {
                MeetResult::Empty
            }
        }
        (Range { lo: a, hi: b }, Range { lo: c, hi: d }) => {
            let lo = (*a).max(*c);
            let hi = (*b).min(*d);
            if lo >= hi {
                MeetResult::Empty
            } else {
                MeetResult::Constraint(Range { lo, hi })
            }
        }
        (Set(s), Set(t)) => {
            let u: BTreeSet<_> = s.intersection(t).cloned().collect();
            if u.is_empty() {
                MeetResult::Empty
            } else if u.len() == 1 {
                MeetResult::Constraint(Point(u.iter().next().unwrap().clone()))
            } else {
                MeetResult::Constraint(Set(u))
            }
        }
        (Set(s), Point(v)) | (Point(v), Set(s)) => {
            if s.contains(v) {
                MeetResult::Constraint(Point(v.clone()))
            } else {
                MeetResult::Empty
            }
        }
        (Set(s), Range { lo, hi }) | (Range { lo, hi }, Set(s)) => {
            let u: BTreeSet<_> = s
                .iter()
                .filter(|x| x.as_int().map_or(false, |i| *lo <= i && i < *hi))
                .cloned()
                .collect();
            if u.is_empty() {
                MeetResult::Empty
            } else if u.len() == 1 {
                MeetResult::Constraint(Point(u.iter().next().unwrap().clone()))
            } else {
                MeetResult::Constraint(Set(u))
            }
        }
    }
}

fn simplify_constraint(c: Constraint) -> Option<Constraint> {
    use Constraint::*;
    match c {
        Range { lo, hi } if lo >= hi => None,
        Set(s) if s.is_empty() => None,
        Set(s) if s.len() == 1 => Some(Point(s.into_iter().next().unwrap())),
        other => Some(other),
    }
}

// ---------------------------------------------------------------------------
// Address
// ---------------------------------------------------------------------------

#[pyclass]
#[derive(Clone)]
pub struct Address {
    pub values: Arc<BTreeMap<Dimension, CoordValue>>,
}

#[pymethods]
impl Address {
    #[new]
    fn new(items: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut map = BTreeMap::new();
        let iter = if items.hasattr("items")? {
            items.getattr("items")?.call0()?.try_iter()?
        } else {
            items.try_iter()?
        };
        for item in iter {
            let pair = item?;
            let dim: Dimension = pair.get_item(0)?.extract()?;
            let val = coord_value_from_py(&pair.get_item(1)?)?;
            map.insert(dim, val);
        }
        Ok(Self {
            values: Arc::new(map),
        })
    }

    fn get(&self, dim: &Dimension) -> Option<PyObject> {
        self.values
            .get(dim)
            .map(|v| Python::with_gil(|py| coord_value_to_py(v, py)))
    }

    /// Tensor API: addr[dim] → value; addr[(d1, d2, ...)] → tuple of values (projection).
    fn __getitem__(&self, key: &Bound<'_, PyAny>, py: Python<'_>) -> PyResult<PyObject> {
        if let Ok(dim) = key.extract::<Dimension>() {
            let v = self
                .values
                .get(&dim)
                .ok_or_else(|| PyKeyError::new_err(dim.name.clone()))?;
            return Ok(coord_value_to_py(v, py));
        }
        let mut dims = Vec::new();
        for item in key.try_iter()? {
            dims.push(item?.extract::<Dimension>()?);
        }
        if dims.is_empty() {
            return Err(PyValueError::new_err(
                "subscript must be Dimension or sequence of Dimensions",
            ));
        }
        let vals: Vec<PyObject> = dims
            .iter()
            .map(|d| {
                self.values
                    .get(d)
                    .ok_or_else(|| PyKeyError::new_err(d.name.clone()))
                    .map(|v| coord_value_to_py(v, py))
            })
            .collect::<PyResult<Vec<_>>>()?;
        let t = pyo3::types::PyTuple::new(py, vals)?;
        Ok(t.into_py(py))
    }

    fn __repr__(&self) -> String {
        let parts: Vec<String> = self
            .values
            .iter()
            .map(|(d, v)| {
                let vstr = match v {
                    CoordValue::Int(i) => i.to_string(),
                    CoordValue::Str(s) => format!("{:?}", s),
                    CoordValue::Bytes(b) => format!("b'{}'", String::from_utf8_lossy(b)),
                };
                format!("{}={}", d.name, vstr)
            })
            .collect();
        format!("Address({})", parts.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Region (conjunction of constraints)
// ---------------------------------------------------------------------------

#[pyclass]
#[derive(Clone)]
pub struct Region {
    pub constraints: Arc<RwLock<BTreeMap<Dimension, Constraint>>>,
}

#[pymethods]
impl Region {
    #[new]
    fn new(constraints: &Bound<'_, PyAny>) -> PyResult<Self> {
        let mut map = BTreeMap::new();
        let dict = constraints.downcast::<pyo3::types::PyDict>()?;
        for key_result in dict.keys().try_iter()? {
            let key = key_result?;
            let dim: Dimension = key.extract()?;
            let val = dict
                .get_item(&key)?
                .ok_or_else(|| PyValueError::new_err("missing dict value"))?;
            let c = constraint_from_py(&val)?;
            map.insert(dim, c);
        }
        Ok(Self::from_map(map))
    }

    fn select(&self, dim: Dimension, constraint: &Bound<'_, PyAny>) -> PyResult<Region> {
        let c = constraint_from_py(constraint)?;
        let mut map: BTreeMap<_, _> = self.constraints.read().unwrap().clone();
        if let Some(existing) = map.remove(&dim) {
            match meet_constraint(&existing, &c) {
                MeetResult::Empty => {
                    return Err(PyValueError::new_err("select: constraint meet is empty"));
                }
                MeetResult::Constraint(merged) => {
                    if let Some(m) = simplify_constraint(merged) {
                        map.insert(dim, m);
                    }
                }
            }
        } else {
            map.insert(dim, c);
        }
        Ok(Region::from_map(map))
    }

    fn shift_range(&self, order_dim: Dimension, delta: i64) -> PyResult<Region> {
        let mut map: BTreeMap<_, _> = self.constraints.read().unwrap().clone();
        if let Some(Constraint::Range { lo, hi }) = map.get(&order_dim) {
            map.insert(
                order_dim,
                Constraint::Range {
                    lo: lo + delta,
                    hi: hi + delta,
                },
            );
        }
        Ok(Region::from_map(map))
    }

    /// Tensor API: region[dim] → constraint (Point/Range/SetC).
    fn __getitem__(&self, dim: &Bound<'_, PyAny>, py: Python<'_>) -> PyResult<PyObject> {
        let d: Dimension = dim.extract()?;
        let c = self
            .constraints
            .read()
            .unwrap()
            .get(&d)
            .cloned()
            .ok_or_else(|| PyKeyError::new_err(d.name.clone()))?;
        constraint_to_py(&c, py)
    }

    /// Tensor API: region[dim] = constraint (meet and update in place).
    fn __setitem__(&self, dim: &Bound<'_, PyAny>, constraint: &Bound<'_, PyAny>) -> PyResult<()> {
        let d: Dimension = dim.extract()?;
        let c = constraint_from_py(constraint)?;
        let mut map = self.constraints.write().unwrap();
        if let Some(existing) = map.remove(&d) {
            match meet_constraint(&existing, &c) {
                MeetResult::Empty => {
                    return Err(PyValueError::new_err(
                        "region[dim]=c: constraint meet is empty",
                    ));
                }
                MeetResult::Constraint(merged) => {
                    if let Some(m) = simplify_constraint(merged) {
                        map.insert(d, m);
                    }
                }
            }
        } else {
            map.insert(d, c);
        }
        Ok(())
    }

    fn constraints_dict(&self, py: Python<'_>) -> PyResult<PyObject> {
        let dict = pyo3::types::PyDict::new(py);
        for (dim, c) in self.constraints.read().unwrap().iter() {
            let key = dim.clone().into_py(py);
            let val = constraint_to_py(c, py)?;
            dict.set_item(key, val)?;
        }
        Ok(dict.into_py(py))
    }

    fn __repr__(&self) -> String {
        let parts: Vec<String> = self
            .constraints
            .read()
            .unwrap()
            .iter()
            .map(|(d, c)| format!("{}:{:?}", d.name, c))
            .collect();
        format!("Region({{{}}})", parts.join(", "))
    }
}

impl Region {
    fn from_map(mut map: BTreeMap<Dimension, Constraint>) -> Self {
        map.retain(|_, c| simplify_constraint(c.clone()).is_some());
        let mut opt_map: BTreeMap<Dimension, Option<Constraint>> =
            map.into_iter().map(|(k, v)| (k, Some(v))).collect();
        for (_, opt) in opt_map.iter_mut() {
            let taken = opt.take().unwrap();
            if let Some(s) = simplify_constraint(taken) {
                *opt = Some(s);
            }
        }
        let map: BTreeMap<_, _> = opt_map
            .into_iter()
            .filter_map(|(k, v)| v.map(|c| (k, c)))
            .collect();
        Self {
            constraints: Arc::new(RwLock::new(map)),
        }
    }
}

fn constraint_from_py(obj: &Bound<'_, PyAny>) -> PyResult<Constraint> {
    if obj.hasattr("value")? && !obj.hasattr("lo")? && !obj.hasattr("values")? {
        let val = obj.getattr("value")?;
        return Ok(Constraint::Point(coord_value_from_py(&val)?));
    }
    if obj.hasattr("lo")? && obj.hasattr("hi")? {
        let lo: i64 = obj.getattr("lo")?.extract()?;
        let hi: i64 = obj.getattr("hi")?.extract()?;
        if lo >= hi {
            return Err(PyValueError::new_err("Range requires lo < hi"));
        }
        return Ok(Constraint::Range { lo, hi });
    }
    if obj.hasattr("values")? {
        let vals = obj.getattr("values")?;
        let set = coord_set_from_py(&vals)?;
        if set.len() > 64 {
            return Err(PyValueError::new_err("SetC cardinality must be <= 64"));
        }
        return Ok(Constraint::Set(set));
    }
    Err(PyValueError::new_err(
        "constraint must be Point, Range, or SetC",
    ))
}

fn constraint_to_py(c: &Constraint, py: Python<'_>) -> PyResult<PyObject> {
    match c {
        Constraint::Point(v) => {
            let p = Point { value: v.clone() };
            Ok(p.into_py(py))
        }
        Constraint::Range { lo, hi } => {
            if *lo >= *hi {
                return Err(PyValueError::new_err(
                    "constraint_to_py: invalid range lo >= hi",
                ));
            }
            let r = Range { lo: *lo, hi: *hi };
            Ok(r.into_py(py))
        }
        Constraint::Set(s) => {
            let set = SetC { values: s.clone() };
            Ok(set.into_py(py))
        }
    }
}

#[pyfunction]
fn canonicalize(region: &Region) -> Region {
    let map: BTreeMap<_, _> = region
        .constraints
        .read()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Region::from_map(map)
}

#[pyfunction]
fn project_prefix(
    addr_or_region: &Bound<'_, PyAny>,
    dims: Vec<Dimension>,
) -> PyResult<Vec<PyObject>> {
    let py = addr_or_region.py();
    if let Ok(addr) = addr_or_region.extract::<Address>() {
        let mut out = Vec::with_capacity(dims.len());
        for d in dims {
            match addr.values.get(&d) {
                Some(v) => out.push(coord_value_to_py(v, py)),
                None => {
                    return Err(PyValueError::new_err(format!(
                        "dimension {} not in address",
                        d.name
                    )))
                }
            }
        }
        return Ok(out);
    }
    if let Ok(region) = addr_or_region.extract::<Region>() {
        let mut out = Vec::with_capacity(dims.len());
        for d in &dims {
            match region.constraints.read().unwrap().get(d) {
                Some(Constraint::Point(v)) => out.push(coord_value_to_py(&v, py)),
                _ => {
                    return Err(PyValueError::new_err(
                        "project_prefix on Region requires all requested dimensions to be Point",
                    ))
                }
            }
        }
        return Ok(out);
    }
    Err(PyValueError::new_err("expected Address or Region"))
}

#[pyfunction]
fn is_point_query(region: &Region) -> bool {
    region
        .constraints
        .read()
        .unwrap()
        .values()
        .all(|c| matches!(c, Constraint::Point(_)))
}

#[pyfunction]
fn is_range_query(region: &Region, order_dim: &Dimension) -> bool {
    let mut has_range = false;
    for (dim, c) in region.constraints.read().unwrap().iter() {
        match c {
            Constraint::Range { .. } if dim == order_dim => has_range = true,
            Constraint::Point(_) => {}
            _ => return false,
        }
    }
    has_range
}

#[pyfunction]
fn engine_protocol_version() -> u32 {
    persisting_engine::PROTOCOL_VERSION
}

/// Optional bincode wire: caller encodes `RpcRequest` / decodes `RpcResponse` (see `persisting-proto`).
#[pyfunction]
fn engine_dispatch(py: Python<'_>, request: &[u8]) -> PyResult<Py<PyBytes>> {
    let out = persisting_engine::dispatch_bytes(request);
    Ok(PyBytes::new(py, &out).into())
}

#[pyfunction]
fn search_embed_text(text: &str, embedding_dim: usize) -> PyResult<Vec<f32>> {
    persisting_engine::agent_search::embed_text(text, embedding_dim).map_err(py_runtime_error)
}

#[pyfunction]
#[pyo3(signature = (dataset, text, id=None, metadata=None, embedding_dim=384))]
fn search_add(
    py: Python<'_>,
    dataset: String,
    text: String,
    id: Option<String>,
    metadata: Option<Bound<'_, PyDict>>,
    embedding_dim: usize,
) -> PyResult<Py<PyAny>> {
    let meta = match metadata {
        None => None,
        Some(d) => Some(pythonize::depythonize(d.as_any())?),
    };
    let req = SearchAddRequest {
        dataset,
        id,
        text,
        metadata: meta,
        embedding_dim,
    };
    let resp = persisting_engine::search_add(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

/// 批量写入文档（内部按 `chunk_size` 切块调用 `SearchAddBatch`，与 CLI `search create` 同引擎路径）。
#[pyfunction]
#[pyo3(signature = (dataset, rows, *, embedding_dim=384, chunk_size=256))]
fn search_add_batch(
    py: Python<'_>,
    dataset: String,
    rows: Bound<'_, PyList>,
    embedding_dim: usize,
    chunk_size: usize,
) -> PyResult<Py<PyAny>> {
    if chunk_size == 0 {
        return Err(PyValueError::new_err("chunk_size must be >= 1"));
    }
    let n = rows.len();
    if n == 0 {
        return Err(PyValueError::new_err("rows must not be empty"));
    }
    let mut total_added = 0usize;
    let mut first_preview: Option<Vec<f32>> = None;
    let mut notes = Vec::new();
    let mut i = 0usize;
    while i < n {
        let end = (i + chunk_size).min(n);
        let mut batch_rows = Vec::with_capacity(end - i);
        for j in i..end {
            let item = rows.get_item(j)?;
            let d = item.downcast::<PyDict>().map_err(|_| {
                PyTypeError::new_err("each row must be a dict with at least key 'text'")
            })?;
            let text: String = d
                .get_item("text")?
                .ok_or_else(|| PyKeyError::new_err("text"))?
                .extract()?;
            let id = match d.get_item("id")? {
                None => None,
                Some(v) => {
                    if v.is_none() {
                        None
                    } else {
                        Some(v.extract::<String>()?)
                    }
                }
            };
            let metadata = match d.get_item("metadata")? {
                None => None,
                Some(v) => {
                    if v.is_none() {
                        None
                    } else {
                        let meta = v
                            .downcast::<PyDict>()
                            .map_err(|_| PyTypeError::new_err("metadata must be a dict or None"))?;
                        Some(pythonize::depythonize(meta.as_any())?)
                    }
                }
            };
            batch_rows.push(SearchAddRequest {
                dataset: dataset.clone(),
                id,
                text,
                metadata,
                embedding_dim,
            });
        }
        let req = SearchAddBatchRequest { rows: batch_rows };
        let resp = persisting_engine::search_add_batch(req).map_err(py_runtime_error)?;
        if resp.status != "ok" {
            return Err(PyRuntimeError::new_err(format!(
                "SearchAddBatch chunk {}..{}: status={} note={}",
                i,
                end.saturating_sub(1),
                resp.status,
                resp.note
            )));
        }
        total_added = total_added.saturating_add(resp.added);
        if first_preview.is_none() {
            first_preview = Some(resp.embedding_preview.clone());
        }
        notes.push(resp.note);
        i = end;
    }
    let out = serde_json::json!({
        "dataset": dataset,
        "added": total_added,
        "embedding_dim": embedding_dim,
        "embedding_preview": first_preview.unwrap_or_default(),
        "status": "ok",
        "note": notes.join(" | "),
    });
    pythonize::pythonize(py, &out)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))
        .map(|x| x.unbind())
}

#[pyfunction]
#[pyo3(signature = (dataset, query, mode="hybrid", k=10, embedding_dim=384, text_column="text", filter=None, nprobes=None, minimum_nprobes=None, maximum_nprobes=None, adaptive_nprobes_margin=None))]
fn search_query(
    py: Python<'_>,
    dataset: String,
    query: String,
    mode: &str,
    k: usize,
    embedding_dim: usize,
    text_column: &str,
    filter: Option<String>,
    nprobes: Option<usize>,
    minimum_nprobes: Option<usize>,
    maximum_nprobes: Option<usize>,
    adaptive_nprobes_margin: Option<f32>,
) -> PyResult<Py<PyAny>> {
    let req = SearchQueryRequest {
        dataset,
        query,
        mode: mode.to_string(),
        k,
        embedding_dim,
        text_column: text_column.to_string(),
        filter,
        nprobes,
        minimum_nprobes,
        maximum_nprobes,
        adaptive_nprobes_margin,
    };
    let resp = persisting_engine::search_query(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
#[pyo3(signature = (dataset, vector_column="embedding", text_column="text", metric="cosine", num_partitions=None, ivf_max_iters=None, ivf_balance_factor=None, ivf_balance_postprocess=None, ivf_postprocess_max_cluster_ratio=None, ivf_sample_rate=None, ivf_target_partition_size=None, ivf_shuffle_partition_batches=None, ivf_shuffle_partition_concurrency=None, pq_num_sub_vectors=None, pq_num_bits=None, pq_max_iters=None, pq_kmeans_redos=None, pq_sample_rate=None))]
fn search_index(
    py: Python<'_>,
    dataset: String,
    vector_column: &str,
    text_column: &str,
    metric: &str,
    num_partitions: Option<usize>,
    ivf_max_iters: Option<usize>,
    ivf_balance_factor: Option<f32>,
    ivf_balance_postprocess: Option<bool>,
    ivf_postprocess_max_cluster_ratio: Option<f32>,
    ivf_sample_rate: Option<usize>,
    ivf_target_partition_size: Option<usize>,
    ivf_shuffle_partition_batches: Option<usize>,
    ivf_shuffle_partition_concurrency: Option<usize>,
    pq_num_sub_vectors: Option<usize>,
    pq_num_bits: Option<u8>,
    pq_max_iters: Option<usize>,
    pq_kmeans_redos: Option<usize>,
    pq_sample_rate: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let req = SearchIndexRequest {
        dataset,
        vector_column: vector_column.to_string(),
        text_column: text_column.to_string(),
        metric: metric.to_string(),
        num_partitions,
        ivf_max_iters,
        ivf_balance_factor,
        ivf_balance_postprocess,
        ivf_postprocess_max_cluster_ratio,
        ivf_sample_rate,
        ivf_target_partition_size,
        ivf_shuffle_partition_batches,
        ivf_shuffle_partition_concurrency,
        pq_num_sub_vectors,
        pq_num_bits,
        pq_max_iters,
        pq_kmeans_redos,
        pq_sample_rate,
    };
    let resp = persisting_engine::search_index(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
fn search_index_list(py: Python<'_>, dataset: String) -> PyResult<Py<PyAny>> {
    let req = SearchIndexListRequest { dataset };
    let resp = persisting_engine::search_index_list(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
fn search_index_delete(py: Python<'_>, dataset: String, index_name: String) -> PyResult<Py<PyAny>> {
    let req = SearchIndexDeleteRequest {
        dataset,
        index_name,
    };
    let resp = persisting_engine::search_index_delete(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
#[pyo3(signature = (dataset, index_name=None, retrain=true, merge_num_indices=None))]
fn search_index_rebuild(
    py: Python<'_>,
    dataset: String,
    index_name: Option<String>,
    retrain: bool,
    merge_num_indices: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let req = SearchIndexRebuildRequest {
        dataset,
        index_name,
        retrain,
        merge_num_indices,
    };
    let resp = persisting_engine::search_index_rebuild(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
#[pyo3(signature = (dataset, pivot_index, target=None, in_place=false))]
fn search_index_reorder(
    py: Python<'_>,
    dataset: String,
    pivot_index: String,
    target: Option<String>,
    in_place: bool,
) -> PyResult<Py<PyAny>> {
    let req = SearchIndexReorderRequest {
        dataset,
        pivot_index,
        target,
        in_place,
    };
    let resp = persisting_engine::search_index_reorder(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
#[pyo3(signature = (target_dataset, source_lance, source_text_column="text", source_id_column=None, embedding_dim=384, limit=None))]
fn search_import_lance(
    py: Python<'_>,
    target_dataset: String,
    source_lance: String,
    source_text_column: &str,
    source_id_column: Option<String>,
    embedding_dim: usize,
    limit: Option<usize>,
) -> PyResult<Py<PyAny>> {
    let req = SearchImportLanceRequest {
        target_dataset,
        source_lance,
        source_text_column: source_text_column.to_string(),
        source_id_column,
        embedding_dim,
        limit,
    };
    let resp = persisting_engine::search_import_lance(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
fn trajectory_append(
    py: Python<'_>,
    storage: String,
    agent_id: String,
    session_id: String,
    records: Bound<'_, PyAny>,
) -> PyResult<Py<PyAny>> {
    let records_ronl: String = if let Ok(s) = records.extract::<String>() {
        s
    } else if let Ok(list) = records.downcast::<PyList>() {
        let mut s = String::new();
        for item in list.iter() {
            let v: serde_json::Value = pythonize::depythonize(item.as_any())?;
            s.push_str(&ron::to_string(&v).map_err(|e| PyRuntimeError::new_err(e.to_string()))?);
            s.push('\n');
        }
        s
    } else {
        return Err(PyTypeError::new_err(
            "records must be str (RONL body) or list of dict-like objects",
        ));
    };
    let req = TrajectoryAppendRequest {
        storage,
        agent_id,
        session_id,
        root_session_id: None,
        records_ronl,
        storage_format: TrajectoryStorageFormat::Auto,
    };
    let resp = persisting_engine::trajectory_append(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
#[pyo3(signature = (storage, agent_id=None, session_id=None, offset=0, limit=None, root_session_id=None))]
fn trajectory_replay(
    py: Python<'_>,
    storage: String,
    agent_id: Option<String>,
    session_id: Option<String>,
    offset: usize,
    limit: Option<usize>,
    root_session_id: Option<String>,
) -> PyResult<Py<PyAny>> {
    let loc = persisting_engine::trajectory::resolve_traj_read_location(
        "trajectory replay",
        storage,
        agent_id,
        session_id,
        root_session_id,
    )
    .map_err(py_runtime_error)?;
    let req = TrajectoryReplayRequest {
        storage: loc.storage,
        agent_id: loc.agent_id,
        session_id: loc.session_id,
        offset,
        limit,
        storage_format: TrajectoryStorageFormat::Auto,
        root_session_id: loc.root_session_id,
    };
    let resp = persisting_engine::trajectory_replay(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

#[pyfunction]
#[pyo3(signature = (storage, agent_id=None, session_id=None, root_session_id=None))]
fn trajectory_stats(
    py: Python<'_>,
    storage: String,
    agent_id: Option<String>,
    session_id: Option<String>,
    root_session_id: Option<String>,
) -> PyResult<Py<PyAny>> {
    let loc = persisting_engine::trajectory::resolve_traj_read_location(
        "trajectory stats",
        storage,
        agent_id,
        session_id,
        root_session_id,
    )
    .map_err(py_runtime_error)?;
    let req = TrajectoryStatsRequest {
        storage: loc.storage,
        agent_id: loc.agent_id,
        session_id: loc.session_id,
        storage_format: TrajectoryStorageFormat::Auto,
        root_session_id: loc.root_session_id,
    };
    let resp = persisting_engine::trajectory_stats(req).map_err(py_runtime_error)?;
    Ok(pythonize::pythonize(py, &resp)
        .map_err(|e| PyRuntimeError::new_err(e.to_string()))?
        .unbind())
}

fn py_runtime_error(error: impl std::fmt::Display) -> PyErr {
    PyRuntimeError::new_err(error.to_string())
}

#[pymodule]
fn _core(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Dimension>()?;
    m.add_class::<Point>()?;
    m.add_class::<Range>()?;
    m.add_class::<SetC>()?;
    m.add_class::<Address>()?;
    m.add_class::<Region>()?;
    m.add_function(wrap_pyfunction!(canonicalize, m)?)?;
    m.add_function(wrap_pyfunction!(project_prefix, m)?)?;
    m.add_function(wrap_pyfunction!(is_point_query, m)?)?;
    m.add_function(wrap_pyfunction!(is_range_query, m)?)?;
    m.add_function(wrap_pyfunction!(engine_protocol_version, m)?)?;
    m.add_function(wrap_pyfunction!(engine_dispatch, m)?)?;
    m.add_function(wrap_pyfunction!(search_embed_text, m)?)?;
    m.add_function(wrap_pyfunction!(search_add, m)?)?;
    m.add_function(wrap_pyfunction!(search_add_batch, m)?)?;
    m.add_function(wrap_pyfunction!(search_query, m)?)?;
    m.add_function(wrap_pyfunction!(search_index, m)?)?;
    m.add_function(wrap_pyfunction!(search_index_list, m)?)?;
    m.add_function(wrap_pyfunction!(search_index_delete, m)?)?;
    m.add_function(wrap_pyfunction!(search_index_rebuild, m)?)?;
    m.add_function(wrap_pyfunction!(search_index_reorder, m)?)?;
    m.add_function(wrap_pyfunction!(search_import_lance, m)?)?;
    m.add_function(wrap_pyfunction!(trajectory_append, m)?)?;
    m.add_function(wrap_pyfunction!(trajectory_replay, m)?)?;
    m.add_function(wrap_pyfunction!(trajectory_stats, m)?)?;
    m.add_function(wrap_pyfunction!(block_io::block_read, m)?)?;
    m.add_function(wrap_pyfunction!(block_io::block_write, m)?)?;
    #[cfg(unix)]
    {
        m.add_class::<mmap_region::MmapRegion>()?;
        m.add_function(wrap_pyfunction!(mmap_region::mmap_reserve, m)?)?;
    }
    #[cfg(target_os = "linux")]
    m.add_function(wrap_pyfunction!(uffd::start_uffd_handler, m)?)?;
    #[cfg(target_os = "macos")]
    m.add_function(wrap_pyfunction!(page_fault_darwin::start_mach_handler, m)?)?;
    m.add_class::<tiered_loop::TieredLoop>()?;
    Ok(())
}
