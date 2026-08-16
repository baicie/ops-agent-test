default: check

fmt:
    cargo fmt

fmt-check:
    cargo fmt --check

rust-test:
    cargo test --locked --all-targets

clippy:
    cargo clippy --locked --all-targets --all-features -- -D warnings

web-install:
    cd web && npm ci

web-test:
    cd web && npm test

web-build:
    cd web && npm run build

demo-test:
    python3 -m unittest discover -s demo/order-service/tests -v

continuity-test:
    cargo test --locked --test continuity --test recovery_fault

remediation-test:
    cargo test --locked --test remediation

ops-test:
    cargo test --locked --test ops --test contracts --test config --test scenario_eval

acceptance-test:
    python3 -m unittest discover -s scripts/tests -v

audit:
    cargo audit
    cargo deny --locked check advisories licenses bans sources
    cd web && npm audit --audit-level=high --registry=https://registry.npmjs.org

test: rust-test web-test demo-test acceptance-test

check: fmt-check clippy test web-build

release-check: web-install check audit
    cargo build --locked --release

release-dry-run: web-build
    cargo build --locked --release
    python3 scripts/release_manifest.py --root . --out dist/release-dry-run \
        --binary target/release/opscodex --web-dir web/dist

build: web-build
    cargo build --locked --release

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
