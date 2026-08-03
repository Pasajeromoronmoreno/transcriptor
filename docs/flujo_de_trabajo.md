# Flujo de trabajo

Este documento fija cómo se trabaja en el repo para que la app de uso diario
nunca se rompa mientras hay desarrollo en curso.

## Reglas duras

1. **`config.toml` y `.env` no se versionan.** Viven en
   `~/.config/transcriptor/`. La configuración pública de referencia vive en
   `config.example.toml`.
2. **`main` refleja lo que corre en la máquina.** Si `main` y el binario
   instalado divergen, se corrige `main`.
3. **Se pushean ramas y tags de versión de forma explícita, nunca en bloque.**

Los tags `archive/master` y `archive/experiment-prompts`, que contenían una API
key de Groq hardcodeada, se borraron el 3 de agosto de 2026. Su historia quedó
respaldada en `~/Documentos/transcriptor-historia-archivada-2026-08-03.bundle`,
que **sigue conteniendo esa key**: no se comparte ni se sube a ningún lado.

## Binario de uso diario

El binario que usás para dictar **no** debe ser el de `target/`, porque
cualquier `cargo build` lo pisa en medio del día. Todo lo que hay bajo `target/`
es salida de compilación descartable; el ejecutable real es el instalado.

```bash
make install
```

Se actualiza a mano, cuando vos querés, después de probar el cambio. Ese es el
punto: la actualización es una decisión, no un efecto secundario de compilar.

El lanzador del escritorio (`~/Documentos/Scripts/transcriptor`) apunta a
`~/.local/bin/transcriptor` y no depende del repositorio.

### La ventana queda en "Finalizado" al salir

Es correcto. El lanzador usa `konsole --hold`, que deja la ventana abierta
cuando el proceso termina para poder leer el último error. La app sale limpia
con Ctrl+C en ~100 ms; los Ctrl+C posteriores no hacen nada porque ya no hay
proceso. Cerrá la ventana normalmente.

## Ramas

Historia lineal, sin merge commits. `main` avanza por fast-forward o por
squash desde un PR.

| Prefijo  | Uso                                      |
| -------- | ---------------------------------------- |
| `feat/`  | funcionalidad nueva                      |
| `fix/`   | corrección de comportamiento             |
| `perf/`  | optimización sin cambio de comportamiento |
| `chore/` | tooling, CI, mantenimiento               |
| `docs/`  | documentación                            |
| `abandoned/` | camino probado que no funcionó       |

Commits en inglés, formato Conventional Commits, atómicos: un commit por
unidad lógica. El patrón que ya venís usando —`docs:` de diseño primero,
después `feat:`/`fix:`, después `test:`— funciona bien y conviene sostenerlo.

### Ramas abandonadas

Una rama que se probó y no funcionó no se borra en silencio ni se deja con
prefijo `feat/`, donde parece trabajo pendiente de mergear. Se renombra a
`abandoned/` y se le adjunta el motivo con `git notes`:

```bash
git branch -m feat/lo-que-sea abandoned/lo-que-sea
git notes add -m 'ABANDONADA — por qué, cuál es el modo de falla, qué queda abierto' abandoned/lo-que-sea
git notes show abandoned/lo-que-sea
```

Así la rama sigue sirviendo para lo único que sirve: que nadie —vos dentro de
seis meses incluido— vuelva a recorrer el mismo camino sin saber que ya se
recorrió. `git notes` no se pushea por defecto; es información local.

## Worktrees

Un worktree por rama activa. Cada uno tiene su propio directorio de trabajo y
su propio `target/`, así compilar en una rama no invalida la caché de otra ni
toca el binario que estás usando.

```bash
# Crear
git worktree add ~/repos/transcriptor-wt/pipeline-cancelation -b feat/pipeline-cancelation main

# Listar
git worktree list

# Eliminar cuando la rama ya se mergeó
git worktree remove ~/repos/transcriptor-wt/pipeline-cancelation
```

### Configuración dentro de un worktree

No hay nada que copiar ni enlazar: tanto `config.toml` como `.env` se resuelven
por XDG (`~/.config/transcriptor/`), así que cualquier worktree funciona de
entrada. Exportar `GROQ_API_KEY` en la shell sigue teniendo prioridad sobre el
archivo, que es lo cómodo para probar con otra key.

## Pull requests

```bash
git push -u origin feat/mi-rama
gh pr create --fill
gh pr checks --watch
gh pr merge --squash --delete-branch
```

La CI (`.github/workflows/ci.yml`) corre build, tests y clippy en cada PR.

Clippy corre con `-D warnings`: cualquier aviso nuevo bloquea el merge. Antes de
abrir un PR conviene correr `make check` (clippy + tests).

El toolchain está fijado en `rust-toolchain.toml` y es la única fuente de
verdad: rustup lo respeta tanto en tu máquina como en el runner. Sin eso, CI
usaba el último stable y local el que estuviera instalado, y como clippy suma
lints en cada versión, `make check` pasaba acá y el mismo commit fallaba allá.
Subir la versión es un commit deliberado.

Un límite conocido: **compila con `--no-default-features`**, porque GTK4 Layer
Shell no está empaquetado en Ubuntu 24.04, que es la imagen del runner. Queda
cubierto config, pipeline, transcripción y observabilidad, que es donde está la
lógica y todos los tests. El overlay se verifica localmente con `cargo build`.

## Volver atrás

El tag `v0.6.0` marca exactamente el código del binario que venías usando.

```bash
git checkout v0.6.0
make install
```

## Logs

La app escribe JSON diario en `~/.local/state/transcriptor/logs/`, con
retención de siete días. Ahí se verifica qué versión arrancó y con qué
configuración: cada arranque registra `version`, `config_path` y `log_path`.
