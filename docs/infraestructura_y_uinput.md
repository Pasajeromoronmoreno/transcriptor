# Infraestructura y Configuración: Virtual Keyboard (uinput)

Este documento detalla la configuración de sistema necesaria para que el Transcriptor pueda simular hardware (teclado/ratón) en Linux, y la hoja de ruta para la portabilidad en el futuro.

## Problema de Fondo
El módulo `uinput` del kernel (User-level Input Subsystem) permite a las aplicaciones del usuario simular dispositivos físicos. Por defecto, en la mayoría de las distribuciones Linux (incluyendo Fedora):
1.  El módulo `uinput` **no se carga** al arrancar el sistema.
2.  El archivo de dispositivo `/dev/uinput` pertenece a `root:root` con permisos `600`, impidiendo que una app común lo use.

## Solución Permanente (Linux)

Para que el programa funcione tras cada reinicio sin intervención manual, hemos configurado:

### 1. Carga Automática del Módulo
Archivo: `/etc/modules-load.d/uinput.conf`
Contenido: `uinput`

**Por qué:** Esto asegura que el kernel habilite el sistema de entrada virtual apenas arranca el sistema, sin esperar a que una aplicación lo "pida" (lo cual requeriría privilegios de administrador que la app no tiene).

### 2. Regla de Permisos udev
Archivo: `/etc/udev/rules.d/99-uinput.rules`
Contenido: `KERNEL=="uinput", GROUP="input", MODE="0660", OPTIONS+="static_node=uinput"`

**Por qué:** 
- `GROUP="input"`: Asigna el dispositivo al grupo de hardware de entrada.
- `MODE="0660"`: Permite lectura/escritura a los miembros de ese grupo (como tu usuario local).
- `OPTIONS+="static_node=uinput"`: Garantiza que el sistema cree el "nodo" del dispositivo con estos permisos incluso antes de que el módulo esté totalmente cargado, evitando "condiciones de carrera" (race conditions).

## Hoja de Ruta: Distribución y Portabilidad

### Para Usuarios Finales en Linux
En lugar de configurar esto a mano, existen dos estrategias:
1.  **Instaladores (.rpm / .deb):** Los paquetes de sistema pueden incluir estos archivos en sus rutas correspondientes. Al instalar el software, el gestor de paquetes (DNF) hace el trabajo sucio.
2.  **Script de "First Run":** Un script de configuración inicial que se ejecute una sola vez con `sudo` para preparar estos archivos.

### Para el Port a Windows
`uinput` es exclusivo de Linux. El plan de desarrollo es:
1.  **Abstracción en Rust:** Crear un `trait` (como una interfaz) llamado `InputSimulator`.
2.  **Implementaciones Específicas:**
    - `LinuxSimulator`: Usa la crate `uinput` (lo que tenemos ahora).
    - `WindowsSimulator`: Usará la API Win32 (`SendInput`) o una librería como `enigo` que ya gestiona las diferencias.
3.  **Compilación Condicional:** Usar `#[cfg(target_os = "linux")]` y `#[cfg(target_os = "windows")]` para que el compilador elija la pieza adecuada automáticamente.
