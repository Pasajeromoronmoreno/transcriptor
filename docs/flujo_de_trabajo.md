# Flujo de trabajo

Este documento fija cómo se trabaja en el repo para que la app de uso diario
nunca se rompa mientras hay desarrollo en curso.

## Reglas duras

1. **Nunca `git push --tags`, `--mirror` ni `--follow-tags`.** Los tags
   `archive/master` y `archive/experiment-prompts` apuntan a la historia vieja
   del proyecto, que contiene una API key de Groq hardcodeada en `config.toml`
   en 29 commits. Ninguna rama actual la contiene, pero esos tags sí. Se
   pushean ramas y tags de versión de forma explícita, nunca en bloque.
2. **`config.toml` y `.env` no se versionan.** La configuración pública de
   referencia vive en `config.example.toml`.
3. **`main` refleja lo que corre en la máquina.** Si `main` y el binario
   instalado divergen, se corrige `main`.

## Binario de uso diario

El binario que usás para dictar **no** debe ser el de `target/`, porque
cualquier `cargo build` lo pisa en medio del día.

```bash
cargo build --release
install -Dm755 target/release/transcriptor ~/.local/bin/transcriptor
```

Se actualiza a mano, cuando vos querés, después de probar el cambio. Ese es el
punto: la actualización es una decisión, no un efecto secundario de compilar.

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

Commits en inglés, formato Conventional Commits, atómicos: un commit por
unidad lógica. El patrón que ya venís usando —`docs:` de diseño primero,
después `feat:`/`fix:`, después `test:`— funciona bien y conviene sostenerlo.

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

`config.toml` ya se resuelve por XDG (`~/.config/transcriptor/config.toml`), así
que funciona en cualquier worktree sin copiar nada.

`.env` es distinto: `dotenvy` lo lee del directorio actual. Enlazalo:

```bash
ln -s ~/repos/transcriptor/.env ~/repos/transcriptor-wt/<rama>/.env
```

Alternativa: exportar `GROQ_API_KEY` en el entorno de la shell, que tiene
prioridad sobre el archivo.

## Pull requests

```bash
git push -u origin feat/mi-rama
gh pr create --fill
gh pr checks --watch
gh pr merge --squash --delete-branch
```

La CI (`.github/workflows/ci.yml`) corre build, tests y clippy en cada PR.

Dos límites conocidos:

- **Compila con `--no-default-features`.** GTK4 Layer Shell no está empaquetado
  en Ubuntu 24.04, que es la imagen del runner. Queda cubierto config,
  pipeline, transcripción y observabilidad, que es donde está la lógica y
  todos los tests. El overlay se verifica localmente con `cargo build`.
- **Clippy todavía no bloquea**: el código arrastra avisos previos. Cuando
  estén saldados, agregar `-- -D warnings` al paso de clippy.

## Volver atrás

El tag `v0.6.0` marca exactamente el código del binario que venías usando.

```bash
git checkout v0.6.0
cargo build --release
install -Dm755 target/release/transcriptor ~/.local/bin/transcriptor
```

## Logs

La app escribe JSON diario en `~/.local/state/transcriptor/logs/`, con
retención de siete días. Ahí se verifica qué versión arrancó y con qué
configuración: cada arranque registra `version`, `config_path` y `log_path`.
