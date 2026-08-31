# Phase Reviews（阶段性 Review）

本目录存放 pChronicle 项目执行阶段的阶段性 review 文档，与 `../specs/`（设计文档）平级、互补：

- **specs/** 记录「要做什么、为什么这样设计」——写于动手之前。
- **reviews/** 记录「现在做得怎么样、下一步往哪走」——写于阶段节点，回看已完成的工作。

## 命名约定

```
YYYY-MM-DD-<topic>-review.md
```

例如：`2026-08-23-product-phase-review.md`。

## 文档结构约定

每篇阶段性 review 建议包含以下章节：

1. **Status** —— 评审时间、覆盖范围（代码区间 / 功能面）、评审人。
2. **结论摘要** —— 一段话总评 + 最重要的 3 个判断。
3. **现状盘点** —— 已建成能力的分层盘点，标注「UI 已暴露 / 后端已有未暴露 / 仅有 spec」。
4. **Review 发现** —— 按层级（产品定位 / 架构 / 界面交互）组织，标注优先级（P0/P1/P2）。
5. **行动项** —— 带 ID、优先级、验收标准的可执行列表，供下一阶段排期引用。
6. **遗留与风险** —— 已知未解决问题、暂缓项及其理由。

## 已有 review

| 日期 | 文档 | 主题 |
|---|---|---|
| 2026-08-23 | [product-phase-review](2026-08-23-product-phase-review.md) | 产品定位、架构与界面整体阶段 review，功能暴露路线 |
| 2026-08-23 | [four-page-prototype-review](2026-08-23-four-page-prototype-review.md) | 四页原型（Overview/Explore/Traces/Analysis）设计 review |
| 2026-08-23 | [frontend-product-review](2026-08-23-frontend-product-review.md) | 运行中前端整体 review（实测截图，对照时间旅行调试器叙事） |
| 2026-08-28 | [pchronicle-design-impl-review](2026-08-28-pchronicle-design-impl-review.md) | `persisting-pchronicle` crate 设计与实现整体 review（分层评分、P0/P1/P2 发现、README 与代码漂移） |

配图统一放在 `assets/` 下，命名 `YYYY-MM-DD-<slug>.svg`（实测截图可用 `assets/YYYY-MM-DD-<slug>/` 子目录放 PNG），文档内以相对路径 `assets/...` 引用。SVG 使用 CSS 变量 + fallback（如 `var(--color-text-primary, #2c2c2a)`），独立浏览器打开和嵌入文档站点均可正常渲染。
