//! Tiered Tensor Address Space (TTAS) — Rust implementation for Persisting.
//! See design: docs/src/design/tensor_address_algebra.md

mod block_io;
#[cfg(unix)]
mod mmap_region;
mod tiered_loop;
#[cfg(target_os = "linux")]
mod uffd;
#[cfg(target_os = "macos")]
mod page_fault_darwin;

use pyo3::exceptions::{PyKeyError, PyValueError};
use pyo3::prelude::*;
use std::collections::BTreeMap;
use std::sync::RwLock;
use std::collections::BTreeSet;
use std::sync::Arc;

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
            _ => return Err(PyValueError::new_err(format!("unknown dimension kind: {}", kind))),
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
            return Err(PyValueError::new_err("subscript must be Dimension or sequence of Dimensions"));
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
                    return Err(PyValueError::new_err("region[dim]=c: constraint meet is empty"));
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
        let mut opt_map: BTreeMap<Dimension, Option<Constraint>> = map
            .into_iter()
            .map(|(k, v)| (k, Some(v)))
            .collect();
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
    Err(PyValueError::new_err("constraint must be Point, Range, or SetC"))
}

fn constraint_to_py(c: &Constraint, py: Python<'_>) -> PyResult<PyObject> {
    match c {
        Constraint::Point(v) => {
            let p = Point { value: v.clone() };
            Ok(p.into_py(py))
        }
        Constraint::Range { lo, hi } => {
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
                    return Err(PyValueError::new_err(format!("dimension {} not in address", d.name)))
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
    m.add_function(wrap_pyfunction!(page_fault_darwin::start_uffd_handler, m)?)?;
    m.add_class::<tiered_loop::TieredLoop>()?;
    Ok(())
}
