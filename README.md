# bysyncify

Bindings and a high-level interface to Binaryen's Asyncify transformation for WebAssembly.

## Overview

[Asyncify](https://kripken.github.io/blog/wasm/2019/07/16/asyncify.html) is a feature of [Binaryen](https://github.com/WebAssembly/binaryen) that allows WebAssembly code to pause and resume execution. This enables async/await style programming in WebAssembly modules that don't natively support it.

`bysyncify` provides Rust bindings and abstractions for working with Asyncify, allowing you to:

- Suspend and resume WebAssembly execution at arbitrary points
- Embed Rust futures within synchronous coroutine code
- Create coroutines that can be driven by async executors

## Features

- **`no_std` compatible**: Works in bare-metal and embedded WebAssembly environments
- **Low-level API**: Direct access to Asyncify primitives via `RawStack`, `RawCore`, and `RawCoroutine`
- **High-level API** (with `alloc` feature): Ergonomic interface with automatic memory management via `Core`, `Coroutine`, and `CoreHandle`
- **Trait implementations**: Implements `awaiter-trait` and `awaiter-trait-02` for interoperability

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
bysyncify = "0.2"
```

To enable heap-allocated types:

```toml
[dependencies]
bysyncify = { version = "0.2", features = ["alloc"] }
```

## Usage

### Prerequisites

Your WebAssembly module must be processed with Binaryen's Asyncify transformation. This can be done using the `wasm-opt` tool:

```bash
wasm-opt --asyncify -O3 input.wasm -o output.wasm
```

The asyncify imports (`asyncify.start_unwind`, `asyncify.stop_unwind`, etc.) must be provided by the host environment.

### Basic Example (with `alloc` feature)

```rust,ignore
use bysyncify::Coroutine;
use core::pin::pin;

// Create a coroutine with a 4KB stack
let coroutine = Coroutine::new(4096, |handle| {
    // Use handle.embed() to await futures within the coroutine
    // let result = handle.embed(pin!(async_operation()));
    
    // Return a value from the coroutine
    42
});

// The coroutine implements Future, so it can be awaited
// let result = coroutine.await;
```

### How It Works

1. **Create a coroutine**: Use `Coroutine::new()` with a stack size and a closure
2. **Embed futures**: Within the closure, use `handle.embed()` to await futures
3. **Suspension**: When a future returns `Poll::Pending`, Asyncify unwinds the stack
4. **Resumption**: When the coroutine is polled again, Asyncify rewinds the stack
5. **Completion**: When the closure returns, the coroutine completes

### Stack Size

The stack size determines how much state can be saved during suspension. A larger stack allows deeper call stacks but uses more memory. 4KB-16KB is typically sufficient for most use cases.

## API Overview

### Low-level Types

- `RawStack`: Memory buffer for Asyncify state
- `RawCore`: Core state manager for Asyncify operations  
- `RawCoroutine<U, T>`: Low-level coroutine wrapper

### High-level Types (requires `alloc`)

- `Core`: Heap-allocated core with automatic memory management
- `Coroutine<U, T>`: High-level coroutine that implements `Future`
- `CoreHandle<'a>`: Handle for embedding futures within coroutines
- `RawCoreHandle`: Raw handle without lifetime constraints

### FFI Functions

- `start_unwind(stack)`: Begin stack unwinding
- `stop_unwind()`: Complete stack unwinding
- `start_rewind(stack)`: Begin stack rewinding
- `stop_rewind()`: Complete stack rewinding
- `get_state()`: Get current Asyncify state (0=normal, 1=unwinding, 2=rewinding)

## License

This project is licensed under [CC0-1.0](https://creativecommons.org/publicdomain/zero/1.0/).

## See Also

- [Binaryen](https://github.com/WebAssembly/binaryen) - WebAssembly toolchain with Asyncify
- [Asyncify blog post](https://kripken.github.io/blog/wasm/2019/07/16/asyncify.html) - Technical overview of Asyncify
- [awaiter-trait](https://crates.io/crates/awaiter-trait) - Trait for awaiter implementations

## Goals
- [ ] Provide safe Rust wrappers for Asyncify
- [ ] Integrate with `awaiter-trait`

## Progress
- [ ] Crate setup (v0.2.6) with `awaiter-trait` integration

---
*AI assisted*
