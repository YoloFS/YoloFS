# ── Third-party backends ──────────────────────────────────────────────

BRANCHFS_OUT := third_party/branchfs/target/release/branchfs
TRY_DIR      := third_party/try
TRY_COMMIT   := $(TRY_DIR)/utils/try-commit

.PHONY: install uninstall

install: install-try install-branchfs

uninstall: uninstall-try uninstall-branchfs

# ── try ───────────────────────────────────────────────────────────────

.PHONY: install-try uninstall-try

$(TRY_DIR)/configure: $(TRY_DIR)/configure.ac
	cd $(TRY_DIR) && autoconf

$(TRY_COMMIT): $(TRY_DIR)/configure $(wildcard $(TRY_DIR)/utils/*.c)
	cd $(TRY_DIR) && ./configure --prefix=/usr/local
	$(MAKE) -C $(TRY_DIR)

install-try: $(TRY_COMMIT)
	sudo $(MAKE) -C $(TRY_DIR) install

uninstall-try:
	sudo rm -f /usr/local/bin/try
	sudo rm -f /usr/local/bin/try-summary
	sudo rm -f /usr/local/bin/try-commit

# ── branchfs ──────────────────────────────────────────────────────────

.PHONY: install-branchfs uninstall-branchfs

$(BRANCHFS_OUT): $(wildcard third_party/branchfs/src/**/*.rs third_party/branchfs/Cargo.toml)
	cargo build --release --manifest-path third_party/branchfs/Cargo.toml

install-branchfs: $(BRANCHFS_OUT)
	sudo install -m 755 $(BRANCHFS_OUT) /usr/local/bin/branchfs

uninstall-branchfs:
	sudo rm -f /usr/local/bin/branchfs
