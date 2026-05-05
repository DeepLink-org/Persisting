//! Lance-backed search：写入文档、建索引、向量/全文/混合查询、IVF 物理重排与 Lance 导入。

pub mod agent;
pub mod lance;

mod ivf_physical_reorder;
