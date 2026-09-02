//! Lance-backed search：写入文档、建索引、向量/全文/混合查询、IVF 物理重排与 Lance 导入。

pub mod find;
pub use find::{
    FindExpr, FindJsonOperator, FindJsonPredicate, FindTextField, FindTextPredicate,
    combine_match_expressions, parse_match_expression,
};

#[cfg(feature = "search")]
pub mod agent;
#[cfg(feature = "lance-store")]
pub mod storyline;
#[cfg(feature = "lance-store")]
pub use storyline::{
    STORYLINE_STEP_SEARCH_COLUMNS, search_storyline_documents_fts,
    search_storyline_step_matches_fts, search_storyline_step_matches_fts_in_columns,
    search_storyline_steps_fts, storyline_steps_fts_available,
};

#[cfg(feature = "search")]
pub mod search_lance;

#[cfg(feature = "search")]
mod ivf_physical_reorder;
