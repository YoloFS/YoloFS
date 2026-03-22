# ── Paper artifact installation ───────────────────────────────────────

PAPER_DIR ?= ../AgFS-paper

.PHONY: install-paper

install-paper:
	cargo build --release -p agfs-bench
	../target/release/agfs-bench install-paper --paper-dir $(PAPER_DIR)

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

install-branchfs:
	tmp=$$(mktemp -d) && cp -a third_party/branchfs/. "$$tmp" && \
	cargo build --release --manifest-path "$$tmp/Cargo.toml" && \
	sudo install -m 755 "$$tmp/target/release/branchfs" /usr/local/bin/branchfs && \
	rm -rf "$$tmp"

uninstall-branchfs:
	sudo rm -f /usr/local/bin/branchfs
