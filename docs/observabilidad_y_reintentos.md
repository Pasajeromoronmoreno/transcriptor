# Plan de observabilidad, errores y reintentos

## Estado de implementación

Implementado en `feat/observability-retries`: logging estructurado en consola y
archivo, retención, diagnósticos de configuración, errores tipados de Groq,
servicio de reintentos configurable, contexto por sesión, pipeline unificado,
estado de retry/error en el overlay y pruebas HTTP locales deterministas.

## Objetivo

Hacer que cada fallo del flujo de grabación pueda responder, con evidencia:

- qué operación falló;
- en qué sesión y en qué intento;
- si el error fue local, de transporte, HTTP, de Groq o de salida;
- si corresponde reintentar;
- qué se mostró al usuario y dónde está el diagnóstico completo.

El sistema debe mantener la consola útil durante el desarrollo, persistir logs
rotados para diagnóstico posterior y alimentar al overlay con mensajes breves.
Nunca debe registrar API keys, audio, prompts completos ni transcripciones.

## Estado actual auditado

La aplicación no tiene una capa de logging. Usa `println!` y `eprintln!` sin
timestamp, nivel, módulo, identificador de sesión ni archivo persistente.

Los problemas principales encontrados son:

1. `api/groq.rs` devuelve `Box<dyn Error>` y el pipeline imprime solamente su
   representación exterior. Un error de `reqwest` pierde en pantalla la cadena
   que distingue timeout, conexión, DNS, TLS o lectura del cuerpo.
2. Los errores HTTP de Groq se convierten en texto libre. No se conservan de
   forma estructurada el status, `x-request-id`, `retry-after` ni una categoría.
3. No existe política de reintentos. El timeout total actual es de 10 segundos.
4. `pipeline/robust.rs` duplica el flujo de transcripción para stop normal y
   stop con Enter invertido, lo que facilita comportamientos divergentes.
5. El auto-split usa `if let Ok(...)` y descarta por completo los errores.
6. Hay resultados ignorados en exportación de audio, persistencia de ganancia,
   canales, limpieza de procesos, socket del overlay y eventos uinput.
7. El overlay recibe solamente `Error de transcripción`, sin categoría,
   intento actual ni referencia para buscar el evento en el log.

## Arquitectura propuesta

### 1. Observabilidad central

Crear `src/observability.rs` e inicializarlo al comienzo del proceso principal.
La implementación recomendada es el ecosistema `tracing`:

- `tracing` para eventos y spans estructurados;
- `tracing-subscriber` para filtros y salida legible a consola;
- `tracing-appender` para escritura no bloqueante y rotación diaria.

Habrá dos salidas simultáneas:

- consola: formato compacto y legible para uso interactivo;
- archivo: eventos estructurados con timestamp, nivel, target y campos.

Ubicación de logs:

1. `$XDG_STATE_HOME/transcriptor/logs`;
2. `~/.local/state/transcriptor/logs` como fallback;
3. directorio temporal solamente si ambos fallan, dejando aviso en consola.

La aplicación debe imprimir al iniciar la ruta efectiva del log. El guard de
escritura no bloqueante debe vivir hasta el final de `main` para no perder los
últimos eventos.

### 2. Errores tipados y códigos estables

Crear errores de dominio en lugar de propagar `Box<dyn Error>` indiscriminado.
Se recomienda `thiserror` para conservar la causa (`source`) y definir códigos
estables, por ejemplo:

- `TRN-AUDIO-SPAWN`, `TRN-AUDIO-READ`, `TRN-AUDIO-EMPTY`;
- `TRN-NET-TIMEOUT`, `TRN-NET-CONNECT`, `TRN-NET-DNS`, `TRN-NET-TLS`;
- `TRN-GROQ-AUTH`, `TRN-GROQ-RATE`, `TRN-GROQ-4XX`, `TRN-GROQ-5XX`;
- `TRN-GROQ-DECODE`;
- `TRN-OUTPUT-CLIPBOARD`, `TRN-OUTPUT-UINPUT`;
- `TRN-CONFIG-READ`, `TRN-CONFIG-PARSE`, `TRN-CONFIG-WRITE`.

Cada error expone:

- `code`: identificador estable para UI y búsqueda;
- `category`: audio, network, provider, output, config o internal;
- `retryable`: decisión explícita, no inferida desde un string;
- `user_message`: mensaje corto y seguro;
- `source`: cadena técnica completa para consola y archivo;
- metadatos seguros como status HTTP, request ID y duración.

### 3. Contexto por sesión

Cada grabación tendrá un `session_id` y un span que abarque:

`recording -> audio_ready -> transcription -> delivery -> completed/error`

Campos mínimos:

- `session_id`;
- modo `push` o `tap`;
- bytes y duración aproximada del WAV;
- intento actual y máximo;
- duración de conexión/transcripción;
- status HTTP y `x-request-id`, si existen;
- resultado final.

No se registran el audio, la API key, el prompt completo ni la transcripción.

### 4. Cliente Groq y clasificación

`api/groq.rs` debe retornar `Result<String, TranscriptionError>` y recibir el
WAV por referencia para poder reconstruir el multipart en cada intento.

Separar explícitamente:

- construcción de request;
- envío/transporte;
- respuesta HTTP;
- lectura del cuerpo;
- decodificación JSON.

En respuestas no exitosas se deben capturar status, `x-request-id`,
`retry-after` y un cuerpo truncado y sanitizado. Un 401 no se debe presentar ni
reintentar como si fuera un problema de red.

Usar timeout de conexión y timeout total separados. Los valores deben ser
configurables y tener defaults razonables.

### 5. Política de reintentos

Crear un `TranscriptionService` que sea dueño de la política. No poner sleeps
sueltos dentro del cliente HTTP ni del pipeline.

Política inicial propuesta:

- máximo 3 intentos;
- espera exponencial: 1 s y 2 s;
- jitter pequeño para evitar reintentos sincronizados;
- respetar `Retry-After` cuando Groq lo envíe;
- conservar el WAV en memoria hasta éxito o fallo definitivo.

Reintentar solamente:

- timeout y fallos transitorios de conexión;
- HTTP 408 y 429;
- HTTP 500, 502, 503 y 504.

No reintentar:

- 400, 401, 403, 404 y 413;
- archivo inválido o vacío;
- error de configuración;
- error de decodificación persistente;
- fallos de clipboard o uinput.

Cada intento genera un evento `warn` con causa, demora elegida e intento. El
fallo final genera un único evento `error` con la cadena completa.

### 6. Integración con overlay y consola

Agregar un estado `Retrying` al overlay. Mensajes sugeridos:

- `Problema de red · reintentando 2/3…`;
- `Groq ocupado · reintentando 2/3…`;
- `Error de autenticación · TRN-GROQ-AUTH`;
- `Error de red · TRN-NET-TIMEOUT`.

El overlay muestra información accionable y breve. Consola y archivo contienen
la causa completa, el `session_id`, el intento y los metadatos HTTP.

### 7. Simplificación del pipeline

Extraer un único flujo `finish_recording(...)` usado tanto por `StopRecording`
como por `StopInvertedEnter`:

1. detener y validar audio;
2. transcribir mediante `TranscriptionService`;
3. aplicar diccionario y formato;
4. entregar a clipboard/uinput;
5. publicar estado final.

`deliver_text` debe devolver `Result`, no terminar silenciosamente. El
auto-split debe usar el mismo servicio y la misma política de errores.

### 8. Configuración

Agregar opciones con defaults, sin exigir cambios al `config.toml` existente:

```toml
[logging]
level = "info"
file = true
retention_days = 7

[retry]
max_attempts = 3
initial_delay_ms = 1000
max_delay_ms = 5000
jitter_ms = 250

[groq]
connect_timeout_ms = 5000
request_timeout_ms = 30000
```

Variables de entorno como `RUST_LOG` pueden sobrescribir temporalmente el nivel
para diagnóstico sin editar configuración.

## Plan de implementación por commits

### Fase 1: infraestructura de logs

1. Agregar dependencias y `observability.rs`.
2. Inicializar consola + archivo al comienzo de `main`.
3. Añadir configuración, creación de directorios y retención.
4. Migrar mensajes de arranque y configuración a eventos estructurados.
5. Verificar que nunca se registre `GROQ_API_KEY`.

### Fase 2: modelo de errores

1. Crear errores tipados y códigos estables.
2. Migrar Groq conservando cadena, status, request ID y `Retry-After`.
3. Migrar audio, configuración, clipboard y uinput progresivamente.
4. Reemplazar descartes relevantes por manejo o evento explícito.

### Fase 3: servicio de transcripción y retry

1. Crear `TranscriptionService` y `RetryPolicy`.
2. Implementar clasificación retryable/no retryable.
3. Implementar backoff, jitter y cancelación limpia al cerrar la app.
4. Emitir eventos por intento y un único error final.

### Fase 4: pipeline y UI

1. Unificar los dos caminos duplicados de finalización.
2. Hacer que `deliver_text` retorne errores.
3. Integrar auto-split con el mismo servicio.
4. Añadir `Retrying` y códigos de error al overlay.

### Fase 5: pruebas y operación

1. Servidor HTTP local de prueba: 500->200, 429->200, timeout->200 y 401.
2. Verificar cantidad de intentos, backoff y ausencia de retry en errores 4xx.
3. Verificar logs persistidos, rotación y redacción de secretos.
4. Verificar overlay para retry y fallo definitivo.
5. Prueba manual completa push/tap con Groq real.
6. Documentar ubicación de logs y comandos de diagnóstico en README.

## Criterios de aceptación

- Un fallo de red indica si fue timeout, conexión, DNS o TLS cuando la librería
  permite distinguirlo, y conserva toda la cadena causal.
- Todo fallo final tiene código, sesión y ubicación de log.
- Los errores transitorios se reintentan según política; los permanentes no.
- El usuario ve el progreso del retry sin recibir detalles técnicos excesivos.
- Consola y archivo permiten reconstruir una sesión completa.
- Ningún secreto ni contenido sensible aparece en logs.
- No quedan rutas de Groq que silencien errores.
- El pipeline no duplica la lógica de finalización.
