BIN := medrec
CARGO ?= cargo
# Where cargo puts what it builds: CARGO_TARGET_DIR when it is set, so the
# binary signed and smoke-tested is the one just built.
TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

.PHONY: build release test lint fmt fmt-check clean install deps smoke verify dev

# Every target that produces a binary signs it (family-cli: an ad-hoc
# signature is a new code identity on every build). medrec reads no
# Keychain item, but the family keeps one rule.
build: SIGN_TARGET = $(TARGET_DIR)/debug/$(BIN)
build:
	$(CARGO) build
	@$(SIGN)

release: SIGN_TARGET = $(TARGET_DIR)/release/$(BIN)
release:
	$(CARGO) build --release
	@$(SIGN)

test:
	$(CARGO) test

lint:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt

fmt-check:
	$(CARGO) fmt --check

clean:
	$(CARGO) clean

deps:
	$(CARGO) fetch

# `--locked` builds from Cargo.lock, the versions the tests and audit ran on.
install: SIGN_TARGET = $${CARGO_INSTALL_ROOT:-$$HOME/.cargo}/bin/$(BIN)
install:
	$(CARGO) install --path . --locked --force
	@$(SIGN)

dev: SIGN_TARGET = $(TARGET_DIR)/debug/$(BIN)
dev:
	$(CARGO) build
	@$(SIGN)

# Offline smoke test: no network, no Keychain, no records folder.
SMOKE_HELP := profiles init people intake review import providers tables refresh timeline search \
	results meds conditions allergies immunizations records documents events bills private labels \
	note images doctor verify reindex views backup drive config auth self-update mcp cache
smoke: build
	@$(TARGET_DIR)/debug/$(BIN) --version >/dev/null
	@$(TARGET_DIR)/debug/$(BIN) info --json >/dev/null
	@$(TARGET_DIR)/debug/$(BIN) completions bash >/dev/null
	@for c in $(SMOKE_HELP); do $(TARGET_DIR)/debug/$(BIN) $$c --help >/dev/null || exit 1; done
	@echo "smoke ok"

verify: fmt-check lint test smoke

define SIGN
if [ -x "$$HOME/Dev/cli-common/scripts/dev-sign.sh" ]; then \
	"$$HOME/Dev/cli-common/scripts/dev-sign.sh" "$(SIGN_TARGET)"; \
else echo "cli-common/scripts/dev-sign.sh not found — $(SIGN_TARGET) left ad-hoc signed"; fi
endef
