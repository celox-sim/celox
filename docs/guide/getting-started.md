# Getting Started

## Prerequisites

- [Rust](https://www.rust-lang.org/tools/install) (edition 2024)
- [Node.js](https://nodejs.org/) (v18+)
- [pnpm](https://pnpm.io/)

## Installation

Clone the repository with submodules:

```bash
git clone --recursive https://github.com/celox-sim/celox.git
cd celox
```

Install Node.js dependencies:

```bash
pnpm install
```

## Building

Build the Rust crates:

```bash
cargo build
```

Build the NAPI bindings (required for TypeScript integration):

```bash
pnpm build:napi
```

Build the TypeScript packages:

```bash
pnpm build
```

## Running Tests

Run all tests (Rust + TypeScript):

```bash
pnpm test
```

Or run Rust and TypeScript tests separately:

```bash
pnpm test:rust    # cargo test
pnpm test:js      # TypeScript tests
```

## Next Steps

- [Writing Tests](./writing-tests.md) -- Learn how to write TypeScript testbenches.
- [Introduction](./introduction.md) -- Overview of the project architecture.
