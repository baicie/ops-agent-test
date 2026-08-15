default: check

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

rust-test:
    cargo test --all-targets

clippy:
    cargo clippy --all-targets --all-features -- -D warnings

web-install:
    cd web && npm ci

web-test:
    cd web && npm test

web-build:
    cd web && npm run build

demo-test:
    python3 -m unittest discover -s demo/order-service/tests -v

test: rust-test web-test demo-test

check: fmt-check clippy test web-build

build: web-build
    cargo build --release

serve *args:
    cargo run -- serve {{args}}

serve-fake *args:
    cargo run -- --fake-model serve {{args}}

web-dev:
    cd web && npm run dev

demo-up:
    docker compose -f demo/docker-compose.yml up --build

demo-down:
    docker compose -f demo/docker-compose.yml down
