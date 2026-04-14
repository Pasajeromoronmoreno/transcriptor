# Transcriptor

Aplicacion de escritorio en Rust para grabar audio con hotkeys globales y transcribirlo via Groq, pensada para flujos de dictado rapidos en Linux.

## Estado

El proyecto esta en uso diario y el flujo principal hoy es `robust`: grabar, soltar, transcribir y pegar el texto en el input activo.

## Requisitos

- Linux
- PulseAudio o PipeWire con compatibilidad Pulse
- Acceso a `uinput` y a los dispositivos de entrada necesarios
- Una API key de Groq

## Configuracion local

1. Copia `config.example.toml` a `config.toml`.
2. Copia `.env.example` a `.env`.
3. Completa `GROQ_API_KEY` dentro de `.env`.
4. Ajusta hotkeys y parametros en `config.toml` segun tu entorno.

`config.toml` y `.env` son locales y no se versionan. La app prioriza `GROQ_API_KEY` si esta disponible.

## Ejecucion

```bash
cargo run
```

## Experimentos

Los scripts de `experiments/prompt_tests/` usan la misma configuracion local y sirven para probar prompts de limpieza y transcripcion.

## Notas

- `audio_exports/` se usa solo para debugging local.
- La configuracion publica de referencia vive en `config.example.toml`.
