PREFIX ?= $(HOME)/.local

.PHONY: build test install uninstall

build:
	cargo build --release

test:
	cargo test

install: build
	install -Dm755 target/release/micloop $(PREFIX)/bin/micloop
	$(PREFIX)/bin/micloop desktop

uninstall:
	-$(PREFIX)/bin/micloop stop
	-$(PREFIX)/bin/micloop desktop --uninstall
	rm -f $(PREFIX)/bin/micloop
