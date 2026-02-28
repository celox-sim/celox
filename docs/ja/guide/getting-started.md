# はじめる

## 前提条件

- [Rust](https://www.rust-lang.org/tools/install)（edition 2024）
- [Node.js](https://nodejs.org/)（v18 以上）
- [pnpm](https://pnpm.io/)

## インストール

サブモジュールを含めてリポジトリをクローンします：

```bash
git clone --recursive https://github.com/tignear/celox.git
cd celox
```

Node.js の依存関係をインストールします：

```bash
pnpm install
```

## ビルド

Rust クレートをビルドします：

```bash
cargo build
```

NAPI バインディングをビルドします（TypeScript 連携に必要）：

```bash
pnpm build:napi
```

TypeScript パッケージをビルドします：

```bash
pnpm build
```

## テストの実行

すべてのテスト（Rust + TypeScript）を実行します：

```bash
pnpm test
```

または、Rust と TypeScript のテストを個別に実行します：

```bash
pnpm test:rust    # cargo test
pnpm test:js      # TypeScript tests
```

## 次のステップ

- [テストの書き方](./writing-tests.md) -- TypeScript テストベンチの書き方を学びます。
- [はじめに](./introduction.md) -- プロジェクトアーキテクチャの概要。
