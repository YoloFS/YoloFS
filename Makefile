# ── Variables ─────────────────────────────────────────────────────────

KDIR             := /lib/modules/$(shell uname -r)
KMOD_OUT         := target/kmod/agfs.ko
KMOD_INSTALL_DIR := $(KDIR)/extra
TARGET_DIR       := $(CURDIR)-target

# ── Build ─────────────────────────────────────────────────────────────

.PHONY: build cli kmod clean

build: cli kmod

$(TARGET_DIR):
	mkdir -p $@

clean:
	rm -rf $(TARGET_DIR)

cli: | $(TARGET_DIR)
	cargo build --release -p agfs

kmod: $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild) | $(TARGET_DIR)
	mkdir -p $(TARGET_DIR)/kmod
	cp kmod/Kbuild $(TARGET_DIR)/kmod/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR)/build M=$(TARGET_DIR)/kmod KBUILD_KMOD_SRC=$(CURDIR)/kmod \
		CONFIG_DEBUG_INFO_BTF_MODULES= modules

# ── Install ───────────────────────────────────────────────────────────

.PHONY: install uninstall

install: cli kmod
	sudo install -m 755 target/release/agfs /usr/local/bin/agfs
	sudo install -d $(KMOD_INSTALL_DIR)
	sudo install -m 644 $(KMOD_OUT) $(KMOD_INSTALL_DIR)/agfs.ko

uninstall:
	sudo rm -f /usr/local/bin/agfs
	sudo rm -f $(KMOD_INSTALL_DIR)/agfs.ko

# ── Test ──────────────────────────────────────────────────────────────

.PHONY: test test-unit test-e2e

test: test-unit test-e2e

test-unit: | $(TARGET_DIR)
	cargo test --release -p agfs --lib

test-e2e: install | $(TARGET_DIR)
	agfs reload
	cargo test --release -p agfs --test e2e -- --test-threads=1
	agfs unload

# ── Lint ──────────────────────────────────────────────────────────────

.PHONY: lint fix

lint: | $(TARGET_DIR)
	cargo fmt --check
	cargo clippy -- -D warnings

fix: | $(TARGET_DIR)
	cargo fmt
	cargo clippy --fix --allow-dirty

# ── Bench ─────────────────────────────────────────────────────────────

.PHONY: bench bench-micro bench-macro

bench: install | $(TARGET_DIR)
	agfs reload
	cargo build --release -p agfs-bench
	./target/release/agfs-bench
	agfs unload

bench-micro: install | $(TARGET_DIR)
	agfs reload
	cargo build --release -p agfs-bench
	./target/release/agfs-bench --micro
	agfs unload

bench-macro: install | $(TARGET_DIR)
	agfs reload
	cargo build --release -p agfs-bench
	./target/release/agfs-bench --macro
	agfs unload

# ── VM ────────────────────────────────────────────────────────────────

.PHONY: vm-%

vm-%:
	./vm.py -- make $*
