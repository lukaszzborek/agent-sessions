# build UI then embed into release binary
build:
    cd ui && npm install --no-audit --no-fund && npm run build
    cd server && cargo build --release

run: build
    ./server/target/release/agent-sessions

# dev: vite with API proxy; run `cargo run` in server separately
dev-ui:
    cd ui && npm run dev
