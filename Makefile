# ── Variables ─────────────────────────────────────────────────────────

KDIR             := /lib/modules/$(shell uname -r)
KMOD_OUT         := local/kmod/yolofs.ko
KMOD_INSTALL_DIR := $(KDIR)/extra
LOCAL_DIR        := $(CURDIR)-local

# ── Build ─────────────────────────────────────────────────────────────

.PHONY: build user kmod

build: user kmod

$(LOCAL_DIR):
	mkdir -p $@

user: | $(LOCAL_DIR)
	cargo build --release -p yolofs

kmod: $(KMOD_OUT)

$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild) | $(LOCAL_DIR)
	mkdir -p $(LOCAL_DIR)/kmod
	cp kmod/Kbuild $(LOCAL_DIR)/kmod/Kbuild
	$(MAKE) -j$(nproc) -C $(KDIR)/build M=$(LOCAL_DIR)/kmod KBUILD_KMOD_SRC=$(CURDIR)/kmod \
		CONFIG_DEBUG_INFO_BTF_MODULES= modules

.PHONY: clean clean-user clean-kmod

clean: clean-user clean-kmod

clean-user:
	cargo clean

clean-kmod:
	rm -rf $(LOCAL_DIR)/kmod

# ── Install ───────────────────────────────────────────────────────────

.PHONY: install uninstall

install: user kmod
	sudo install -m 4755 -o root local/target/release/yolo /usr/local/bin/yolo
	sudo install -d $(KMOD_INSTALL_DIR)
	sudo install -m 644 $(KMOD_OUT) $(KMOD_INSTALL_DIR)/yolofs.ko

uninstall:
	sudo rm -f /usr/local/bin/yolo
	sudo rm -f $(KMOD_INSTALL_DIR)/yolofs.ko

# ── Test ──────────────────────────────────────────────────────────────

.PHONY: test test-unit test-e2e

test: test-unit test-e2e

test-unit: | $(LOCAL_DIR)
	cargo test --release -p yolofs --lib

test-e2e: install | $(LOCAL_DIR)
	yolo reload
	cargo test --release -p yolofs --test e2e -- --test-threads=1
	yolo unload

# ── Lint ──────────────────────────────────────────────────────────────

.PHONY: lint fix

lint: | $(LOCAL_DIR)
	cargo fmt --check
	cargo clippy --release -- -D warnings

fix: | $(LOCAL_DIR)
	cargo fmt
	cargo clippy --release --fix --allow-dirty

# ── VM ────────────────────────────────────────────────────────────────

.PHONY: vm-%

vm-%:
	./vm.py -- make $*
