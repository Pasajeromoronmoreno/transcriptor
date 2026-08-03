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

La configuración vive en `~/.config/transcriptor/`, fuera del repositorio, para
que el binario instalado no dependa de la copia del código:

1. `mkdir -p ~/.config/transcriptor`
2. Copia `config.example.toml` a `~/.config/transcriptor/config.toml`.
3. Copia `.env.example` a `~/.config/transcriptor/.env` y completa
   `GROQ_API_KEY` ahí (`chmod 600` recomendado).
4. Ajusta hotkeys y parametros en `config.toml` segun tu entorno.

Tanto `config.toml` como `.env` se buscan en este orden: junto al binario,
después en `~/.config/transcriptor/` y por último subiendo desde el directorio
actual. La app prioriza `GROQ_API_KEY` del entorno si esta disponible.

## Ejecucion

Para desarrollo:

```bash
cargo run
```

Para instalarlo como programa de escritorio:

```bash
make install
```

Eso deja el ejecutable en `~/.local/bin/transcriptor`. Todo lo que hay bajo
`target/` es salida de compilación descartable: el único ejecutable "real" es el
instalado, y `make install` es el puente entre los dos.

Sólo puede correr una instancia a la vez: el proceso toma un candado en
`$XDG_RUNTIME_DIR/transcriptor.lock` y un segundo arranque se rechaza con un
mensaje claro en vez de pelear por el micrófono y el teclado virtual.

## Atajos

| Atajo | Efecto |
| --- | --- |
| Modificador + trigger (corto) | Dictado en modo Tap |
| Modificador + trigger (sostenido) | Dictado en modo Push |
| `ESC` | Cancela el dictado en curso: descarta el audio y aborta la llamada a la API sin pegar nada |

Las teclas concretas se configuran en la sección `[hotkeys]`.

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
