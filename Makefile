.PHONY: all node pruntime e2e test clippy

all: node pruntime e2e

node:
	cargo build --release
pruntime:
	make -C standalone/pruntime
e2e:
	make -C e2e/res
	cd e2e && yarn build:proto
	cd frontend/packages/sdk && yarn && yarn build && rm -rf node_modules
test:
	cargo test --workspace --exclude node-executor --exclude phala-node

clippy:
	cargo clippy --tests
	make clippy -C standalone/pruntime

lint:
	cargo dylint --all --workspace
