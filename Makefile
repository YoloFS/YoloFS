# ── Variables ─────────────────────────────────────────────────────────

KDIR             := /lib/modules/$(shell uname -r)
KMOD_OUT         := target/kmod/agfs.ko
KMOD_INSTALL_DIR := $(KDIR)/extra
BTF_VMLINUX      := $(KDIR)/build/vmlinux
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

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild) $(BTF_VMLINUX) | $(TARGET_DIR)
	mkdir -p $(TARGET_DIR)/kmod
	cp kmod/Kbuild $(TARGET_DIR)/kmod/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR)/build M=$(TARGET_DIR)/kmod KBUILD_KMOD_SRC=$(CURDIR)/kmod modules

$(BTF_VMLINUX): /sys/kernel/btf/vmlinux
	sudo cp $< $@

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
	cargo test -p agfs --lib

test-e2e: install
	agfs reload
	cargo test -p agfs --test e2e -- --test-threads=1
	agfs unload

# ── Lint ──────────────────────────────────────────────────────────────

.PHONY: lint fix

lint:
	cargo fmt --check
	cargo clippy -- -D warnings

fix:
	cargo fmt
	cargo clippy --fix --allow-dirty

# ── Bench ─────────────────────────────────────────────────────────────

.PHONY: bench bench-micro bench-macro

bench: install
	agfs reload
	cargo build --release -p agfs-bench
	./target/release/agfs-bench
	agfs unload

bench-micro: install
	agfs reload
	cargo build --release -p agfs-bench
	./target/release/agfs-bench --micro
	agfs unload

bench-macro: install
	agfs reload
	cargo build --release -p agfs-bench
	./target/release/agfs-bench --macro
	agfs unload

# ── VM ────────────────────────────────────────────────────────────────

.PHONY: vm-%

vm-%:
	./vm.py -- make $*
