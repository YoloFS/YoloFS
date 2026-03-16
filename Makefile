# ── Variables ─────────────────────────────────────────────────────────

KDIR            ?= /lib/modules/$(shell uname -r)/build
KMOD_OUT         := target/kmod/agfs.ko
KMOD_INSTALL_DIR := /lib/modules/$(shell uname -r)/extra
TARGET_DIR       := $(CURDIR)-target

# ── Build ─────────────────────────────────────────────────────────────

.PHONY: build clean cli kmod lint fix

BTF_SRC  := /sys/kernel/btf/vmlinux
BTF_DEST := /usr/lib/modules/$(shell uname -r)/build/vmlinux

build: cli kmod

$(TARGET_DIR):
	mkdir -p $@

cli: | $(TARGET_DIR)
	cargo build --release

kmod: $(BTF_DEST) $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild) | $(TARGET_DIR)
	mkdir -p $(TARGET_DIR)/kmod
	cp kmod/Kbuild $(TARGET_DIR)/kmod/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR) M=$(TARGET_DIR)/kmod KBUILD_KMOD_SRC=$(CURDIR)/kmod modules

$(BTF_DEST): $(BTF_SRC)
	sudo cp $(BTF_SRC) $(BTF_DEST)

clean:
	rm -rf $(TARGET_DIR)

lint:
	cargo fmt --check
	cargo clippy -- -D warnings

fix:
	cargo fmt
	cargo clippy --fix --allow-dirty

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

.PHONY: test test-unit test-e2e

test: test-unit test-e2e

test-unit:
	cargo test --lib

test-e2e: install
	agfs init
	cargo test --test e2e -- --test-threads=1

# ── Bench ─────────────────────────────────────────────────────────────

.PHONY: bench

bench: install
	agfs load
	cargo build --release --bin agfs-bench
	./target/release/agfs-bench
	agfs unload

# ── VM ────────────────────────────────────────────────────────────────

.PHONY: vm-%

vm-%:
	./vm.py -- make $*

# ── CI ────────────────────────────────────────────────────────────────

.PHONY: ci

ci: lint install
	agfs load
	$(MAKE) test-unit
	$(MAKE) test-e2e
	agfs unload
