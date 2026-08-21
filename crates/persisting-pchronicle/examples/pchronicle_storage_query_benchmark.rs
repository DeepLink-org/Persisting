// Keep the user-facing release example and the benchmark suite on one
// implementation so their metrics cannot drift apart.
include!("../benches/lance_vs_json.rs");
