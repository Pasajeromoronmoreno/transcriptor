# Un solo ejecutable: `target/` es salida de compilación descartable y
# `~/.local/bin/transcriptor` es el programa instalado, que es al que apunta el
# lanzador del escritorio. `make install` es el único puente entre los dos.
PREFIX ?= $(HOME)/.local
CONFIG_DIR ?= $(HOME)/.config/transcriptor

.PHONY: build test lint check install clean

build:
	cargo build --release

test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings

check: lint test

install: build
	install -Dm755 target/release/transcriptor $(PREFIX)/bin/transcriptor
	@echo "Instalado en $(PREFIX)/bin/transcriptor"
	@echo "Configuración y API key: $(CONFIG_DIR)/{config.toml,.env}"

clean:
	cargo clean
