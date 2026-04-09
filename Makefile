# ── Variables ─────────────────────────────────────────────────────────

KDIR             := /lib/modules/$(shell uname -r)
BUILD_DIR        := $(CURDIR)/build
KMOD_OUT         := $(BUILD_DIR)/yolofs.ko
KMOD_INSTALL_DIR := $(KDIR)/extra
USER_BIN         := $(CURDIR)/target/release/yolo

# ── Build ─────────────────────────────────────────────────────────────

.PHONY: build user kmod

build: user kmod

$(BUILD_DIR):
	mkdir -p $@

user: | $(BUILD_DIR)
	cargo build --release -p yolofs

kmod: $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild) | $(BUILD_DIR)
	cp kmod/Kbuild $(BUILD_DIR)/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR)/build M=$(BUILD_DIR) KBUILD_KMOD_SRC=$(CURDIR)/kmod \
		CONFIG_DEBUG_INFO_BTF_MODULES= modules

.PHONY: clean clean-user clean-kmod

clean: clean-user clean-kmod

clean-user:
	rm -rf "$(CURDIR)/target"/*

clean-kmod:
	rm -rf "$(BUILD_DIR)"/*

# ── Install ───────────────────────────────────────────────────────────

.PHONY: install uninstall

install: user kmod
	sudo install -m 4755 -o root $(USER_BIN) /usr/local/bin/yolo
	sudo install -d $(KMOD_INSTALL_DIR)
	sudo install -m 644 $(KMOD_OUT) $(KMOD_INSTALL_DIR)/yolofs.ko

uninstall:
	sudo rm -f /usr/local/bin/yolo
	sudo rm -f $(KMOD_INSTALL_DIR)/yolofs.ko

# ── Test ──────────────────────────────────────────────────────────────

.PHONY: test test-unit test-e2e

test: test-unit test-e2e

test-unit: | $(BUILD_DIR)
	cargo test --release -p yolofs --lib

test-e2e: install | $(BUILD_DIR)
	yolo reload
	cargo test --release -p yolofs --test e2e -- --test-threads=1
	yolo unload

# ── Lint ──────────────────────────────────────────────────────────────

.PHONY: lint fix

lint: | $(BUILD_DIR)
	cargo fmt --check
	cargo clippy --release -- -D warnings

fix: | $(BUILD_DIR)
	cargo fmt
	cargo clippy --release --fix --allow-dirty

# ── VM ────────────────────────────────────────────────────────────────

.PHONY: vm-%

vm-%:
	./vm.py -- make $*
