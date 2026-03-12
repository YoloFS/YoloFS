.PHONY: all build cli kmod install uninstall insmod rmmod clean test test-unit test-integration

all: install insmod

build: cli kmod

cli:
	cargo build --release --manifest-path cli/Cargo.toml

install: cli
	sudo install -m 4755 -o root cli/target/release/agfs /usr/local/bin/agfs

uninstall:
	sudo rm -f /usr/local/bin/agfs

kmod:
	$(MAKE) -C kmod

insmod: kmod rmmod
	sudo insmod kmod/agfs.ko

rmmod:
	sudo rmmod agfs || true

clean:
	cargo clean --manifest-path cli/Cargo.toml
	$(MAKE) -C kmod clean

test: test-unit test-integration

test-unit:
	cargo test --lib --manifest-path cli/Cargo.toml

test-integration: install insmod
	cargo test --manifest-path cli/Cargo.toml --test integration -- --test-threads=1
