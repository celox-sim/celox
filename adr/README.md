# Architecture Decision Records

プロジェクト内部の設計意思決定を時系列で記録する。

## Format

`NNNN-title.md` — 連番 + kebab-case タイトル。

各 ADR は以下のセクションを持つ:

- **Date**: 作成日
- **Status**: `proposed` / `accepted` / `superseded` / `deprecated`
- **Context**: 背景・課題
- **Decision**: 決定事項
- **Consequences**: 結果・影響

Status が変わった場合は ADR 内に履歴を追記する。
superseded の場合は後継 ADR へのリンクを記載する。

## Index

| # | Title | Date | Status |
|---|-------|------|--------|
| [0001](0001-value-change-callback-gpi-vpi.md) | Value Change Callback / GPI / VPI | 2026-03-07 | proposed |
| [0002](0002-external-modules.md) | External Modules | 2026-03-07 | proposed |
| [0004](0004-software-modules-sequential-first.md) | Software Modules Are Sequential-First | 2026-04-12 | proposed |
