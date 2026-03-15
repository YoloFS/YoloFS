BINARIES = \
	lint_fix/lint-fix \
	config_validator/validate \
	optimizer/optimize

all: $(BINARIES)

lint_fix/lint-fix: lint-fix.c
	$(CC) -o $@ $<

config_validator/validate: validate.c
	$(CC) -o $@ $<

optimizer/optimize: optimize.c
	$(CC) -o $@ $<

clean:
	rm -f $(BINARIES)

.PHONY: all clean
