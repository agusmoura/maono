# Panel de Omarchy para el Maono PD100W — diseño

Fecha: 2026-10-07 · Fork: `agusmoura/maono`, rama `omarchy-panel`, que sale de `wired-0417`.
Revisión 2: incorpora la revisión de Hermes (34 ítems, veredicto "aprobar con cambios"). Ver §10.

## 1. Objetivo

Controlar todo lo que el PD100W permite, más la cadena de PipeWire que lo limpia, desde un
widget de la barra de Omarchy. El widget tiene que verse coherente con los paneles nativos,
manejarse con teclado y tener perfiles que cambien todo de una vez.

**Lo que pidió Agus**

- "Todas las capas de personalización, ajustes y perfiles" en el plugin.
- Fork propio.
- Buenas prácticas de UX/UI para plugins de Omarchy.
- Todas las características que se puedan sacar del mic.
- "Nada hardcodeado".

**Decisiones cerradas**

- Usos: llamadas, streaming/Discord, grabación y gaming. Cada uno tiene un perfil de fábrica.
- Un perfil guarda y aplica el mic y el filtro de PipeWire.
- Fork público, más un PR upstream (shahriyardx/maono#1).
- Interfaz en español e inglés.
- `maono serve` es el único dueño del mic. El panel tiene pestañas.
- El EQ real va en PipeWire. El DSP del mic queda aparte, como "Experimental".
- Compresor por software con `lsp-plugins-ladspa` (ya instalado).
- El monitoreo y el volumen de auriculares del mic van en una sección chica y plegada.
- Fuente por defecto: `maono_clean`.
- Monitor "Live": escuchar en tiempo real la fuente limpia o la cruda por la salida por defecto (agregado el 2026-10-07).

**"Nada hardcodeado"**

Rangos, opciones, nombres de modos, colores, presets de EQ, perfiles y textos salen de datos
embebidos y de la configuración del usuario, no de literales en la UI. La distribución del
panel y el gráfico del EQ se diseñan a mano. No hay un generador genérico de formularios ni
descriptores externos de otros modelos.

**Criterios de éxito**

1. Cada control cambia el audio o la luz de verdad, o está marcado como experimental.
2. Aplicar un perfil desde el panel, la CLI o un atajo deja **las claves que el perfil incluye** en el mismo estado.
3. Los botones físicos se ven en el panel en menos de 1 s.
4. Desenchufar y volver a enchufar el mic no rompe nada. La reaplicación nunca desmutea.
5. `cargo test`, `node --test` y `omarchy plugin validate` pasan, incluidos los escenarios de §8.

## 2. Hechos del hardware

El mapa completo está en `docs/protocol/pd100w.md`, sacado de Maono Link 3.8.52 por
ingeniería inversa. La calibración del 2026-10-07 se agrega como §8 de ese mismo documento,
con método y resultados.

| Estado | Qué |
|---|---|
| Confirmado escribiendo | mute, ganancia 0–20, supresión on/off + nivel 0–2, luz on/off, brillo 1–100, efecto (0 fijo, 1 respiración, 2 ciclo), color 0–7, colores propios (H×100 hasta 35900 sin problema de signo, S y V ×100, cantidad en 0x208E, color 8+k), 0x0045 = 1 apaga el stream del medidor |
| Confirmado leyendo | batería 0x0042, cargando 0x0049, firmware 0x000B (108 → v1.0.8), serie 0x001E–0x002D, slot 0x2000 en el mic por cable |
| Sin efecto en el audio USB | EQ interno de 7 slots (probado con SET simple, múltiple y por rango, con y sin 0x20B4) y reverb |
| Sin confirmar | compresor y limitador internos, monitoreo 0x20AF, volumen de auriculares 0x207F, persistencia tras un apagado |
| Nunca se escribe | reset 0x100A, serie, firmware, segundo bloque EQ 0x2046–0x2068, 0x20B4, cualquier id fuera de la allowlist |

Comportamiento del canal HID:

- Un SET de la PC no genera notificación. El tipo 0x03 solo llega cuando se apretan los botones físicos.
- Las respuestas GET llegan a todos los descriptores abiertos.
- Varias tramas pueden llegar juntas en un mismo reporte.

## 3. Arquitectura

```
 Omarchy shell                                     backend: binario `maono` (Rust)
┌───────────────────────────────┐  stdin JSONL   ┌─────────────────────────────────────┐
│ Service.qml (kind "service",  │ ─────────────▶ │ maono serve  = único dueño del mic   │
│   una sola instancia)         │ ◀───────────── │  ├ device  (hidraw, lector de        │
│ BarWidget.qml → Loader Panel  │  stdout JSONL  │  │   eventos, allowlist)             │
│ Model.js · i18n/{es,en}.json  │                │  ├ profiles (~/.config/maono)        │
└───────────────────────────────┘                │  ├ filter  (conf + pw-cli + verif.)  │
   CLI / atajos ──▶ $XDG_RUNTIME_DIR/maono.sock ─▶│  └ socket Unix para la CLI           │
                                                 └─────────────────────────────────────┘
```

### 3.1 Dueño único y concurrencia

- `serve` toma un `flock` exclusivo sobre `$XDG_RUNTIME_DIR/maono.lock` y escucha en
  `$XDG_RUNTIME_DIR/maono.sock`. Por el socket corre el mismo protocolo JSONL que por stdin.
- La CLI (`maono set`, `maono profile apply`, …) primero intenta el socket. Si no hay serve,
  toma el mismo lock y habla directo con el hidraw. Si el lock está tomado pero el socket no
  responde, falla con un mensaje claro. El TUI sigue la misma regla.
- Dentro de serve, todas las operaciones pasan por una sola cola. Aplicar un perfil no se
  mezcla con otras escrituras.

### 3.2 Backend (`src/`)

| Módulo | Responsabilidad |
|---|---|
| `mic.rs` | **Transporte:** tramas 0x03 (simple y múltiple), 0x04, 0x05 y 0x06. Un lector que separa las tramas concatenadas y reparte cada una a las peticiones pendientes o al estado (notificaciones de botones). Distingue `WouldBlock`, EOF/desconexión y errores reales. Sin `sleep` bloqueante en el loop de serve |
| `safety.rs` | **Barrera única:** allowlist inmutable compilada (id → tipo, rango, paso y enums permitidos). Valida cada campo de cualquier SET (simple, múltiple o por rango) antes de enviar nada. Rechaza en vez de recortar en silencio y devuelve el valor efectivo. Ningún flag (`--raw` desaparece para `set`) ni descriptor la amplía. `get` y `scan` siguen siendo de solo lectura |
| `descriptor.rs` + `devices/pd100w.json` (embebido) | Metadatos para la UI: `key` con punto, id, tipo, rango y paso, `encoding` (`raw`, `db10_2000`, `x100`), opciones con clave de traducción, `group` y `status` (`verified`/`experimental`/`readonly`). Un test garantiza que cada campo escribible del descriptor está dentro de la allowlist |
| `device.rs` | Selección del dispositivo: `352f:0417` (cable) o `352f:0414` (receptor, solo con los campos verificados por upstream; lo experimental queda oculto). Si aparecen los dos, se usa el de cable y el panel lo avisa. Antes de reaplicar nada se confirma la identidad (VID/PID en 0x0016/0x0017) |
| `filter.rs` | Cadena de PipeWire (§3.4) |
| `profiles.rs` | Perfiles (§3.5) |
| `serve.rs` | Loop principal, cola, reconexión con backoff, socket |
| `main.rs` | CLI: `status`, `get`, `scan`, `set key=value…`, `schema`, `profile list/apply/save/rename/duplicate/delete`, `filter …`, `source clean/raw`, `serve`, `shell install/uninstall`. El TUI se mantiene |

### 3.3 Estado y configuración

| Archivo | Contenido | Quién lo escribe |
|---|---|---|
| `~/.config/maono/config.json` | `version`, `activeProfile`, `applyOnReconnect` | serve |
| `~/.config/maono/filter.json` | estado completo del filtro (fuente de verdad del conf generado) | serve |
| `~/.config/maono/profiles/<id>.json` | `{version, id, name, mic:{…}, filter:{…}, light:{…}}`; las claves son opcionales | serve |
| `~/.config/maono/eq/<id>.json` | presets de EQ del usuario | serve |
| `~/.config/pipewire/filter-chain.conf.d/maono-clean.conf` | generado a partir de `filter.json` | serve |
| entrada del widget en `~/.config/omarchy/shell.json` | preferencias de la UI (§5.3) | solo el panel |

Todas las escrituras son atómicas: se escribe un archivo temporal y después se hace `rename`.
El `version` de cada archivo permite migraciones. Un archivo corrupto se ignora con un aviso;
nunca se borra.

### 3.4 Filtro de PipeWire

**Contrato fijo:** mono, 48 kHz, captura con `node.dont-fallback` y `node.linger`.

**Nodo de captura:** se descubre la `Audio/Source` cuyas propiedades USB son `352f:0417`
(o `0414`) y se usa su `node.name` actual. No se arma concatenando el número de serie.

**Grafo fijo, con bypass en vivo:**

```
captura ─▶ HPF (2 × bq_highpass, 24 dB/oct) ─▶ banda 1..5 ─▶ LPF (bq_lowpass)
        ─▶ RNNoise (noise_suppressor_mono) ─▶ compresor LSP mono ─▶ maono_clean
```

- **Un solo pasa-altos.** Lo fija el preset de EQ y el perfil lo puede pisar. Se edita en un único lugar, la pestaña EQ. El "×2" es la pendiente de esa misma etapa.
- **Bypass sin reiniciar:**
  - biquads: ganancia 0 dB o frecuencia en el extremo;
  - RNNoise: VAD 0 y su control de mezcla, si existe en la versión instalada;
  - LSP: su control de bypass.
  - Los nombres reales de los controles se toman del plugin instalado y quedan en una tabla de datos con unidades y conversiones. Ejemplo: el umbral de LSP es amplitud lineal; se guarda en dB y se convierte.
- **Cambios en vivo:** `pw-cli set-param <nodo> Props`. Después se escribe `filter.json` y se regenera el conf, con debounce de 1 s y sin reiniciar. Así lo que se cambió en vivo sobrevive al reinicio.
- **Cambios estructurales:** reinician `filter-chain.service` una sola vez por operación (un perfil agrupa todo). Son tres casos:
  - cambiar el tipo de una banda (peak/shelf cambia el `label`);
  - instalar o desinstalar un plugin;
  - el primer arranque.
  - El panel avisa que el audio se corta ~1 s.
- **Dependencias ausentes** (LSP o RNNoise): esa etapa no se genera. La UI la muestra deshabilitada, con la línea de instalación. Un perfil que la pide se aplica sin ella y el ack lo dice explícitamente: "aplicado sin compresor: falta lsp-plugins-ladspa".
- **Verificación tras cargar:** el nodo `maono_clean` existe y sus Props tienen los valores esperados. Si falla, se restaura el conf anterior, se reinicia y se reporta. Nunca se da éxito solo porque systemd dice "active".
- **Monitor en vivo:** `serve` corre un `pw-loopback` (mono) desde `maono_clean` o desde el mic crudo hacia la salida por defecto.
  - Nunca se guarda: arranca apagado y se corta al salir `serve`.
  - Si la salida por defecto no parece auriculares (puerto, `form_factor` o ícono), se niega, salvo `force`, porque por parlantes se acopla.
  - Por Bluetooth se escucha con ~150–250 ms de retraso.
- **Fuente por defecto:** `maono source clean|raw` usa `pactl set-default-source` y verifica el resultado. WirePlumber la persiste por su cuenta. No cambia las apps que fijaron su propia entrada (Discord, por ejemplo); el panel lo aclara.

### 3.5 Perfiles

- **Identidad:** `id` estable (slug `[a-z0-9-]`, que es el nombre del archivo) separado de `name`, que se muestra. Se rechazan las colisiones.
- **Renombrar y borrar:** renombrar cambia solo `name`. Borrar el perfil activo deja `activeProfile` vacío. Duplicar crea un id nuevo.
- **Perfiles parciales:** solo se aplican las claves presentes. `dirty` compara únicamente esas claves, con valores normalizados (nunca batería, serie ni preferencias de UI).
- **Los perfiles nunca guardan `mute`.** Mute es un control en vivo, no parte de un perfil.
- **Color de luz:** se guarda como valor, `{"preset":"red"}` o `{"hsv":[h,s,v]}`, no como índice de slot. Al aplicar, el backend busca o crea el slot propio. Así, borrar un color propio no rompe ningún perfil.
- **Aplicar:**
  1. Validar todo el perfil contra la allowlist y la tabla del filtro. Si algo es inválido, no se toca nada.
  2. Escribir el mic campo por campo, releyendo cada valor.
  3. Aplicar el filtro: en vivo, o con un único reinicio si es estructural.
  4. Si todo salió bien, `activeProfile` = id.
  5. Si algo falló, el ack trae `{"ok":false,"applied":[…],"failed":[…]}`, el estado queda `partial` y `activeProfile` no cambia. El filtro anterior se restaura si es posible. La atomicidad del hardware no se promete.
- **Guardar:** guarda el estado **confirmado**, nunca el optimista. "Guardar estado actual como…" incluye por defecto mic + filtro + luz, y la UI permite desmarcar grupos.
- **Primera ejecución:** se copian los perfiles de fábrica y se **lee** el estado del mic. No se aplica nada en silencio.
- **Reconexión física** (el mic desaparece y vuelve con serve corriendo): si `applyOnReconnect` está activo, se reaplica el perfil activo. Arrancar serve o recargar el shell solo sincroniza, nunca reaplica. Si había cambios sin guardar (`dirty`), se reaplica el perfil y el panel ofrece "Recuperar cambios", con los valores confirmados previos guardados en memoria.

## 4. Protocolo JSONL de `serve` (versión 1)

- Un mensaje por línea. stdout lleva solo JSONL; los logs van a stderr.
- Al arrancar: `hello`, después `state` completo.
- Cada comando lleva `id`. El `ack` se manda después de confirmar (relectura o verificación del filtro) e incluye los valores efectivos, seguido de un `state` si algo cambió.
- Una petición sin respuesta en 3 s da `ack` con `ok:false` y `error:"timeout"`.
- stdin cerrado (EOF): serve suelta el lock y sale.

Comandos:

```json
{"id":1,"cmd":"set","changes":{"mic.gain":14,"light.brightness":60}}
{"id":2,"cmd":"filter.set","changes":{"eq.bands.0.gain":3,"rnnoise.vad":80}}
{"id":3,"cmd":"filter.bypass","stage":"all","on":true}
{"id":4,"cmd":"source.default","which":"clean"}
{"id":5,"cmd":"profile.apply","id":"llamada"}
{"id":6,"cmd":"profile.save","name":"Mi voz","groups":["mic","filter","light"],"overwrite":null}
{"id":7,"cmd":"profile.rename","id":"mi-voz","name":"Voz de noche"}
{"id":8,"cmd":"profile.duplicate","id":"llamada"}
{"id":9,"cmd":"profile.delete","id":"mi-voz"}
{"id":10,"cmd":"eq.preset.save","name":"Mi EQ"}
{"id":11,"cmd":"light.custom","op":"add","hsv":[330,1,1]}
{"id":12,"cmd":"config.set","changes":{"applyOnReconnect":false}}
{"id":14,"cmd":"monitor.set","on":true,"source":"clean","force":false}
{"id":13,"cmd":"refresh"}
```

Eventos:

```json
{"ev":"hello","protocol":1,"schema":{},"presets":[],"profiles":[],"config":{}}
{"ev":"state","device":"connected","mic":{},"filter":{},"deps":{"lsp":true,"rnnoise":true},"activeProfile":"llamada","dirty":false,"partial":null}
{"ev":"changed","key":"mic.mute","value":true,"source":"button"}
{"ev":"ack","id":5,"ok":true,"effective":{},"warnings":["…"]}
{"ev":"profiles","profiles":[]}
{"ev":"error","code":"permission","hint":"udev"}
```

`device` puede valer `connected`, `disconnected`, `unsupported` o `permission`. Un valor que
el mic todavía no informó es `null`, no 0. Un valor fuera del rango de escritura se muestra
tal cual y no se corrige solo.

## 5. Panel

### 5.1 Estructura

Ancho `Style.space(420)`, como el plugin de los Sony, con `fittedContentWidth`/`Height`.
Alto máximo 640, con scroll.

**Cabecera (`PanelHero`):**

- nombre, batería (⚡ si carga) y firmware;
- medidor de nivel nativo (`PwNodePeakMonitor`) sobre la **fuente seleccionada como predeterminada**. Se rotula así porque no garantiza lo que recibe una app que fijó otra entrada. Avisa clipping cerca de 0 dBFS;
- interruptor de mute;
- interruptor **Live** con chips **Limpio / Crudo** (para comparar A/B). Si la salida no son auriculares, avisa y pide confirmación antes de forzar;
- selector de perfil con un punto si hay cambios sin guardar, y la acción "Reaplicar perfil" (descarta los cambios).

**Pestañas** (con glifo y etiqueta corta; se eligen con `1`–`6`, o `h`/`l` sobre la tira):

| # | Pestaña | Contenido |
|---|---|---|
| 1 | Voz | Ganancia (slider que ignora la rueda) y supresión del mic (off/baja/media/alta). **Plegado:** "Auriculares del mic", con monitoreo (ninguno/voz/PC/ambos, representando el valor leído aunque sea 7) y volumen 0–20, ambos marcados experimentales |
| 2 | EQ | Bypass del EQ, chips de presets (los 7 de Maono y los propios), curva en Canvas, pasa-altos y pasa-bajos. Plegado "Bandas": tipo, frecuencia, ganancia y Q de las 5 bandas. "Guardar como preset" |
| 3 | Filtro | Interruptor general del filtro (bypass total) y fuente por defecto limpia/cruda. RNNoise: on/off, umbral VAD 0–99, gracia, y gracia retroactiva con la nota "agrega latencia". Compresor: on/off, umbral, ratio, ataque, release. Las dependencias ausentes muestran la línea de instalación |
| 4 | Luz | On/off, efecto, brillo, 8 colores y colores propios (hasta 5): alta con editor HSV, edición y borrado. Con efecto "ciclo", la selección de color se deshabilita |
| 5 | Perfiles | Lista con aplicar, duplicar, renombrar y borrar (borrar con confirmación en 2 pasos). "Guardar estado actual como…" con casillas mic/filtro/luz. Interruptor "Reaplicar al reconectar". Comando para atajos (copiable) |
| 6 | ⚙ Ajustes | Acciones de la barra, paso de la rueda, ocultar si está desconectado, batería en la barra, idioma, "Mostrar DSP experimental del mic", info del equipo (firmware, serie, hidraw, nodo de PipeWire) y diagnóstico |

Con "experimental" activado aparece, en Voz, la sección plegada **"DSP del mic (no
verificado)"**: compresor y limitador internos, reverb y EQ interno (±12 dB). Todo lleva la
insignia "no verificado". Lo que no tiene un rango justificable queda de solo lectura.

### 5.2 Convenciones

- `KeyboardPanel` y `PanelKeyCatcher`. Un solo cursor para mouse y teclado, que no se pinta hasta la primera tecla o el primer hover. El cursor sigue el auto-scroll y se reajusta al plegar, borrar o cambiar de pestaña.
- Teclas: `j`/`k` filas, `h`/`l` ajustar, Enter/Espacio activar, Esc cerrar, Tab cambiar al panel vecino. Con un editor de texto o número abierto (renombrar, frecuencia, Q), `blocked` queda activo: Enter confirma y Esc cancela.
- Estilo: `Style.space()`, `Style.font.*`, `Color.*` y `bar.foreground`. El texto atenuado usa `Qt.darker(fg, 1.4)`. Los únicos colores literales son los swatches de la luz, que salen del descriptor.
- Estructura: `PanelSectionHeader` en mayúsculas y `PanelSeparator`. Pie con atajos en `font.caption` al 40 % de opacidad.
- Sliders: ignoran la rueda dentro del scroll. Debounce de 120 ms.
- **Escritura optimista por clave**, con id de petición. Solo se combinan cambios seguidos de la misma clave. Un ack viejo no pisa una edición más nueva. Si llega un error o una desconexión, la clave vuelve al último valor confirmado.
- **Sin conexión:** Perfiles y Ajustes siguen funcionando. Los controles del mic se ven deshabilitados (no desaparecen). Un valor desconocido se muestra como "—", nunca como 0 ni como "desmuteado".
- Hay una línea de ayuda accionable para cada caso: falta la regla udev, filter-chain caído ("Iniciar"), falta LSP o RNNoise, receptor detectado.

### 5.3 Barra, preferencias e IPC

- **Acciones** configurables en ⚙ y en `shell.json`: clic izquierdo abre el panel, clic medio mutea, clic derecho pasa al perfil siguiente, la rueda cambia la ganancia ± paso.
- **Preferencias de la UI:** se guardan con `bar.shell.updateEntryInline(<id del plugin>, entrada)`. La entrada se manda **completa**: los settings actuales fusionados con los cambios locales pendientes. Hay un caché local que se refresca cuando cambian los settings desde afuera. Rust no escribe `shell.json`.
- **IPC** (target literal `maono`): `toggle`, `open`, `close`, `toggleMute`, `nextProfile` y `applyProfile(id)`, para los atajos de Hyprland. Siguen funcionando con el mic desconectado: devuelven el error, no se cuelgan.
- **Service:** el manifest declara `kinds: ["service","bar-widget"]` con `entryPoints.service`. El widget obtiene la instancia compartida con la búsqueda de servicio propio del shell, así que hay un solo `serve` aunque haya varios monitores. El Service no es `keepLoaded`: un hot-reload reinicia serve, que solo sincroniza (§3.5). Los cambios del Service requieren `omarchy restart shell`.

### 5.4 Instalación y migración

- `maono shell install` copia el conjunto completo y explícito de archivos (manifest, Service, BarWidget, Panel, Model.js, i18n) a una carpeta temporal, la valida (`omarchy plugin validate`) y la mueve a `~/.config/omarchy/plugins/io.github.agusmoura.maono/` con `rename`. Así, el hot-reload nunca ve una copia a medio hacer.
- La primera vez desactiva el widget viejo `maono` y pone el nuevo en su lugar de la barra. Antes avisa y muestra el comando para revertirlo.
- El binario va a `~/.local/bin/maono` (`cargo install --path .`).

## 6. Perfiles de fábrica

Son valores iniciales; quedan como archivos editables. Ninguno incluye mute.

| Perfil | Mic | Filtro | Luz |
|---|---|---|---|
| Llamada | ganancia 20, NR baja | preset Original + HPF 90 Hz, RNNoise VAD 85 %, compresor −24 dB 3:1 | — |
| Streaming / Discord | ganancia 20, NR baja | preset Stream1 + HPF 90 Hz, RNNoise VAD 80 %, compresor −20 dB 4:1 | on, fijo, color del tema |
| Grabación | ganancia 18, NR off | preset Original + HPF 70 Hz, RNNoise VAD 0 (denoise sin compuerta), compresor off | — |
| Gaming | ganancia 20, NR media | preset Game1 + HPF 100 Hz, RNNoise VAD 92 %, compresor −20 dB 4:1 | — |

"Color del tema" se resuelve al guardar el archivo de fábrica a partir del acento de Omarchy y
queda guardado como HSV.

## 7. Seguridad

- La allowlist inmutable de `safety.rs` es la única barrera, y la usan todos los caminos de escritura.
- No hay escritura cruda.
- El descriptor no puede ampliar la allowlist, y un test lo verifica.
- Cada entrada se valida por tipo, rango, paso y enum. Una entrada inválida se rechaza, no se recorta en silencio.
- Las pruebas de escritura sobre el mic real se hacen en una etapa aparte, autorizada explícitamente por Agus.

## 8. Pruebas

**Rust** (`cargo test`, con un trait `Transport` falso):

- Tramas: SET simple, múltiple y por rango; separación de tramas concatenadas; EOF y desconexión.
- Allowlist: todo id/valor inválido se rechaza; el descriptor no excede la allowlist.
- Concurrencia: socket contra lock; dos clientes a la vez; un ack viejo frente a una edición nueva.
- Perfiles: validación previa, aplicación parcial con `failed`, `dirty` sobre las claves incluidas, slug y colisiones, escritura atómica, color por valor.
- Reconexión: arrancar serve no reaplica; la reconexión física sí; nunca se desmutea.
- Filtro: generación del conf (snapshot), dependencia ausente, verificación y rollback cuando el nodo no aparece, agrupación de un solo reinicio.

**JS** (`node --test`): `Model.js`, que cubre textos e idioma, la curva del EQ, el optimismo por clave y la fusión de la entrada de `shell.json`.

**Plugin:**

- `omarchy plugin validate`.
- `omarchy restart shell`, `omarchy-shell maono open`, captura con `grim` de cada pestaña.
- Manual: foco, scroll y teclado.

**Con el mic real** (autorizado aparte): botones físicos, desenchufar y enchufar, aplicar cada perfil y escuchar.

## 9. Fuera de alcance

- Paquete AUR.
- Cambio de perfil automático según la app.
- Descriptores externos y otros modelos.
- Actualización de firmware.
- Mapa propio del receptor (0x3000).

## 10. Cambios por la revisión de Hermes

Se incorporaron:

- **Seguridad y concurrencia:** dueño único con socket y lock (1); Service compartido (2); allowlist inmutable sin escritura cruda (3); identidad del dispositivo y elección entre cable y receptor (10).
- **Perfiles:** contrato de aplicación y fallo parcial (4); reaplicar sin desmutear y solo en la reconexión física (5); perfiles parciales y `dirty` (6); reglas de datos (7); color propio por valor (25).
- **Protocolo e IO:** validación por tipo y rango (8); lector de eventos (9); JSONL completo (11); optimismo por clave (12); hot-reload (13); un solo medidor nativo (14).
- **Filtro:** persistencia de los cambios en vivo (15); reinicios agrupados y bypass en vivo (16); verificación y rollback (17); contrato mono/48k y unidades (18); dependencias ausentes (19); un solo pasa-altos (20); tipo de banda estructural (21); nodo por propiedades USB (22); fuente por defecto (23).
- **UI e instalación:** monitoreo con "ninguno" y experimental (24); `updateEntryInline` completo (26); instalación y migración (27); 420 px y la pestaña "Filtro" en lugar de "Ruido" (28); teclado en los editores (29); bypass y "Reaplicar perfil" (30).
- **Pruebas y documentación:** pruebas de riesgos (31); suplemento de calibración (32); sin descriptores externos (33); estados sin conexión (34).

Con matices:

- (10) El receptor no queda "no soportado": upstream verificó los campos básicos en 0x2000 a través del receptor, así que se soportan esos y lo experimental se oculta.
- (19) Ante una dependencia ausente se eligió "aplicar sin esa etapa y avisar explícitamente", en vez de rechazar todo el perfil.
