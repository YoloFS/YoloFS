# ── Variables ─────────────────────────────────────────────────────────

KDIR            ?= /lib/modules/$(shell uname -r)/build
KMOD_OUT        := kmod/build/agfs.ko
KMOD_INSTALL_DIR := /lib/modules/$(shell uname -r)/extra

# ── Build ─────────────────────────────────────────────────────────────

.PHONY: build clean cli kmod

build: cli kmod

cli:
	cargo build --release

kmod: $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild)
	mkdir -p kmod/build
	cp kmod/Kbuild kmod/build/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR) M=$(CURDIR)/kmod/build modules

clean:
	cargo clean
	rm -rf kmod/build

# ── Install ───────────────────────────────────────────────────────────

.PHONY: install uninstall

install: cli kmod
	sudo install -m 4755 -o root target/release/agfs /usr/local/bin/agfs
	sudo install -d $(KMOD_INSTALL_DIR)
	sudo install -m 644 $(KMOD_OUT) $(KMOD_INSTALL_DIR)/agfs.ko

uninstall:
	sudo rm -f /usr/local/bin/agfs
	sudo rm -f $(KMOD_INSTALL_DIR)/agfs.ko

# ── Test ──────────────────────────────────────────────────────────────

.PHONY: test test-unit test-e2e lint fix

test: test-unit test-e2e

test-unit:
	cargo test --lib

test-e2e: install
	agfs init
	cargo test --test e2e -- --test-threads=1

lint:
	cargo fmt --check
	cargo clippy -- -D warnings

fix:
	cargo fmt
	cargo clippy --fix --allow-dirty

# ── Bench ──────────────────────────────────────────────────────────────

.PHONY: bench

bench: install
	agfs load
	cargo build --release --bin agfs-bench
	./target/release/agfs-bench
	agfs unload

# ── CI ─────────────────────────────────────────────────────────────────

.PHONY: ci

ci: lint install
	agfs load
	$(MAKE) test-unit
	$(MAKE) test-e2e
	agfs unload
