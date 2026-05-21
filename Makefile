# ── Variables ─────────────────────────────────────────────────────────

KVER             := $(shell uname -r)
KDIR             := /lib/modules/$(KVER)
KMOD_OUT         := $(CURDIR)/build/$(KVER)/yolofs.ko
USER_OUT         := $(CURDIR)/target/release/yolo

# ── Build ─────────────────────────────────────────────────────────────

.PHONY: build user kmod

build: user kmod

user: $(USER_OUT)
$(USER_OUT): $(shell find user -name '*.rs' -type f 2>/dev/null) Cargo.toml Cargo.lock
	cargo build --release

kmod: $(KMOD_OUT)
$(KMOD_OUT): $(wildcard kmod/*.c kmod/*.h kmod/Kbuild)
	mkdir -p $(@D)
	ln -sf $(CURDIR)/kmod/Kbuild $(@D)/Kbuild
	$(MAKE) -j$$(nproc) -C $(KDIR)/build M=$(@D) KBUILD_KMOD_SRC=$(CURDIR)/kmod \
		CONFIG_DEBUG_INFO_BTF_MODULES= modules

# ── Install ───────────────────────────────────────────────────────────

.PHONY: install install-user install-kmod

install: install-user install-kmod

install-user: $(USER_OUT)
	sudo install -m 4755 -o root $(USER_OUT) /usr/local/bin/yolo

install-kmod: $(KMOD_OUT)
	sudo install -d $(KDIR)/extra
	sudo install -m 644 $(KMOD_OUT) $(KDIR)/extra/yolofs.ko

# ── Clean ─────────────────────────────────────────────────────────────

.PHONY: clean clean-user clean-kmod

clean: clean-user clean-kmod

clean-user:
	rm -rf "$(CURDIR)/target"

clean-kmod:
	rm -rf "$(CURDIR)/build"

# ── Test ──────────────────────────────────────────────────────────────

.PHONY: test test-vm test-unit test-e2e test-e2e-vm

test: test-unit test-e2e
test-vm: test-unit test-e2e-vm

test-unit:
	cargo test --release --lib

test-e2e: install
	sudo sysctl -w kernel.dmesg_restrict=0 >/dev/null
	yolo reload
	trap 'yolo unload' EXIT; \
	cargo test --release --test e2e -- --test-threads=1

test-e2e-vm: $(USER_OUT)
	./vm.py -- make kmod install
	./vm.py -- sudo sysctl -w kernel.dmesg_restrict=0 >/dev/null
	./vm.py -- yolo reload
	trap './vm.py -- yolo unload' EXIT; \
	cargo --config 'target."cfg(all())".runner = "./vm.py --"' \
		test --release --test e2e -- --test-threads=1

# ── Lint ──────────────────────────────────────────────────────────────

.PHONY: lint fix

lint:
	cargo fmt --check
	cargo clippy --release -- -D warnings

fix:
	cargo fmt
	cargo clippy --release --fix --allow-dirty
