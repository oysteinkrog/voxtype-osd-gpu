# Dev loop for voxtype-osd-gpu. Timings and reasons: docs/dev-loop.md.
# Every cargo call goes through heavy-build, the machine's build admission.

set shell := ["bash", "-cu"]

hb := "heavy-build"
fork := env("VOXTYPE_FORK", home_directory() / "work/voxtype")

# List the recipes.
default:
    @just --list --unsorted

# Type-check everything: libs, tests, examples.
check:
    {{hb}} cargo check --workspace --all-targets

# CPU tests (no gpu_* binaries, no examples). Extra args go to nextest: `just test words`.
test *args:
    {{hb}} cargo nextest run --workspace --lib --tests {{args}}

# CPU tests of one crate: `just t vo-core`, `just t vo-render slug`.
t crate *args:
    {{hb}} cargo nextest run -p {{crate}} --lib --tests {{args}}

# Assemble the WGSL glyph shader and validate it with naga.
shaders:
    {{hb}} cargo nextest run -p vo-render --lib -E 'test(/^text::slug_gpu::tests::/)'

# GPU tests (golden images), offscreen. Prints the contact sheet path.
test-gpu *args:
    {{hb}} cargo nextest run --workspace --profile gpu {{args}}

# Rewrite the goldens from the current output. Look at the contact sheet before committing.
golden-update:
    VO_GOLDEN_UPDATE=1 HEAVY_BUILD_ALLOW_RERUN=1 {{hb}} cargo nextest run --workspace --profile gpu

# Doc tests, which nextest does not run.
doctest:
    {{hb}} cargo test --workspace --doc

# Run a phase 0 example in release: `just bench p0_slug`, `just bench p0_shape`.
bench example="p0_slug" *args:
    {{hb}} cargo run --release --example {{example}} -- {{args}}

# Clippy on all targets with warnings as errors, then the rustfmt check.
lint:
    {{hb}} cargo clippy --workspace --all-targets -- -D warnings
    cargo fmt --all --check

# Budget gate: own heap, cold first frame and frames while idle, offline and live in a private
# nested KWin. Limits and options are env vars, listed at the top of scripts/budget.sh.
budget:
    {{hb}} cargo build --release -p vo --example toast -p vo-host-wayland --example fake_click
    scripts/budget.sh

# Preview and scrub an example in a window, replayed to the same time after Rust edits (agents: nested KWin only).
preview example *args:
    {{hb}} cargo build -p vo --bin vo-preview --features preview
    VO_PREVIEW_BUILD="{{hb}} cargo" target/debug/vo-preview run {{example}} {{args}}

# Contact sheet of an example: `just sheet title_card out.png --frames 12 [--check golden.png]`.
sheet example out *args:
    {{hb}} cargo build --release -p vo --example {{example}}
    target/release/examples/{{example}} --sheet {{out}} {{args}}

# Seek timings of an example: forward play, backwards scrub, random frames.
preview-bench example:
    {{hb}} cargo build --release -p vo --example {{example}}
    target/release/examples/{{example}} --bench-seek

# Format the workspace.
fmt:
    cargo fmt --all

# The whole local gate: check, lint, CPU tests, doc tests, GPU tests, budget.
all: check lint test doctest test-gpu budget

# voxtype fork: type-check only the GTK overlay binary.
osd-check:
    cd {{fork}} && {{hb}} cargo check --features osd-gtk4 --bin voxtype-osd-gtk4

# voxtype fork: build the GTK overlay for a quick look (dev profile, no LTO).
osd-build:
    cd {{fork}} && {{hb}} cargo build --features osd-gtk4 --bin voxtype-osd-gtk4
    @echo "binary: {{fork}}/target/debug/voxtype-osd-gtk4"

# voxtype fork: unit tests of the overlay modules (`osd::` in the lib, no features, so it
# shares the build with every other `cargo test --lib` there).
osd-test *args:
    cd {{fork}} && {{hb}} cargo test --lib osd:: {{args}}
