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

### Overlay global

El overlay forma parte del build normal y requiere GTK4 y GTK4 Layer Shell:

```bash
sudo dnf install gtk4-devel gtk4-layer-shell-devel
cargo run
```

La OSD se ejecuta como un proceso separado del grabador, no toma foco y muestra
los estados de armado, grabacion Push/Tap, latch, transcripcion, pegado y error.
### Logs, errores y reintentos

La aplicación escribe eventos legibles en consola y logs JSON diarios en
`$XDG_STATE_HOME/transcriptor/logs` o `~/.local/state/transcriptor/logs`.
Al iniciar informa la ruta efectiva. Los archivos se conservan siete días por
defecto y nunca incluyen la API key, audio, prompt completo o transcripción.

Cada grabación tiene un `session_id`. Los fallos muestran un código estable en
el overlay y guardan en el log la causa técnica, intento, status HTTP y request
ID de Groq cuando están disponibles. Timeout, 408, 429 y errores transitorios
5xx se reintentan con backoff; autenticación y otros 4xx no.

Las secciones opcionales `[logging]` y `[retry]`, además de los timeouts bajo
`[groq]`, están documentadas en `config.example.toml`. `RUST_LOG` permite subir
temporalmente el nivel de diagnóstico.

## Experimentos

Los scripts de `experiments/prompt_tests/` usan la misma configuracion local y sirven para probar prompts de limpieza y transcripcion.

## Notas

- `audio_exports/` se usa solo para debugging local.
- La configuracion publica de referencia vive en `config.example.toml`.
