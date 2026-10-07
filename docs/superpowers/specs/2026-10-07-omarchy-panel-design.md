# Panel de Omarchy para el Maono PD100W — diseño

Fecha: 2026-10-07 · Fork: `agusmoura/maono`, rama `omarchy-panel` (sale de `wired-0417`)

## 1. Objetivo

Controlar todo lo que el PD100W permite, más la cadena de PipeWire que lo limpia, desde un
widget de la barra de Omarchy. Tiene que ser coherente con los paneles nativos, manejable
con teclado y con perfiles que cambien todo de una vez.

**Lo que pidió Agus**

- "Todas las capas de personalización, ajustes y perfiles" en el plugin.
- Fork propio.
- Buenas prácticas de UX/UI para plugins de Omarchy.
- Todas las características que se puedan sacar del mic.
- "Nada hardcodeado".

**Decisiones cerradas en la conversación**

- Usos: llamadas, streaming/Discord, grabación y gaming. Cada uno trae un perfil de fábrica.
- Un perfil guarda y aplica el estado del mic y el filtro de PipeWire.
- Fork público, más un PR chico upstream con el soporte por cable (shahriyardx/maono#1).
- Interfaz en español e inglés.
- Arquitectura: `maono serve` (un proceso con una sola conexión al mic) y un panel con pestañas.
- Ecualizador real en PipeWire. Los controles DSP del mic van aparte, como "Experimental".
- Compresor por software con `lsp-plugins-ladspa` (paquete oficial de Arch).
- Monitoreo y volumen de auriculares del mic: sección chica y plegada (Agus usa los XM5).
- Fuente por defecto: `maono_clean`.

**Qué significa "nada hardcodeado"**

Todo esto sale de datos y no de literales en el código de la UI: rangos, opciones, nombres
de modos, colores, presets de EQ, perfiles y textos. La distribución del panel y el gráfico
del EQ se diseñan a mano. No se construye un generador genérico de formularios.

**Criterios de éxito**

1. Cada control del panel cambia el audio o la luz de verdad, o está marcado como experimental.
2. Aplicar un perfil desde el panel, la CLI o un atajo deja el mic y el filtro en el mismo estado.
3. Los botones físicos del mic se reflejan en el panel en menos de 1 s.
4. Desenchufar y volver a enchufar el mic no rompe nada. Si así está configurado, se reaplica el perfil activo.
5. `cargo test`, `node --test` y `omarchy plugin validate` pasan.

## 2. Hechos del hardware (calibración del 2026-10-07)

El mapa completo está en el informe de ingeniería inversa (Maono Link 3.8.52, `libPD100XW.dylib`).
Resumen de lo verificado sobre el mic real:

| Estado | Qué |
|---|---|
| Confirmado escribiendo | mute, ganancia 0–20, supresión on/off + nivel 0–2, luz on/off, brillo 1–100, efecto 0 fijo / 1 respiración / 2 ciclo, color 0–7, colores propios (H×100 hasta 35900 sin problema de signo, S y V ×100, cantidad en 0x208E, color 8+k), 0x0045 = 1 apaga el stream del medidor |
| Confirmado leyendo | batería 0x0042, cargando 0x0049, firmware 0x000B (108 → v1.0.8), serie 0x001E–0x002D, slot 0x2000 para el mic por cable |
| Sin efecto observado en el audio USB | EQ de 7 slots (escrito con SET simple, SET múltiple y escritura por rango, con y sin el flag 0x20B4) y reverb |
| Sin conclusión | compresor y limitador; persistencia tras apagar el mic |
| Nunca se expone | reset de fábrica 0x100A, escritura de serie, firmware, segundo bloque de EQ 0x2046–0x2068, ids sin mapear |

**Comportamiento del canal HID que condiciona el diseño**

- Un SET enviado por la PC **no** genera notificación. Las notificaciones de tipo 0x03 son solo de los botones físicos.
- Las respuestas GET (0x04) llegan a **todos** los descriptores abiertos. Por eso, todo escritor relee el valor después de escribir, y `serve` se entera aunque escriba otro proceso (CLI o atajo).

## 3. Arquitectura

```
 Omarchy shell (Quickshell)                       backend (Rust, binario `maono`)
┌──────────────────────────────┐   stdin JSONL   ┌──────────────────────────────┐
│ Service.qml  (1 instancia)    │ ──────────────▶ │ maono serve                   │
│  └ Process "maono serve"      │ ◀────────────── │  ├ device: hidraw, GET/SET,    │
│ BarWidget.qml (icono, acciones)│  stdout JSONL  │  │   lista de bloqueo + clamp  │
│ Panel.qml (pestañas)          │                 │  ├ profiles: ~/.config/maono   │
│ Model.js (lógica pura)        │                 │  └ filter: conf + pw-cli       │
│ i18n/es.json, en.json         │                 └──────────────────────────────┘
└──────────────────────────────┘    CLI / atajos: `maono profile apply llamada`, `maono set …`
```

### 3.1 Backend (`src/`)

| Módulo | Responsabilidad |
|---|---|
| `mic.rs` (existe) | Tramas 0x03/0x04. Suma SET múltiple y lectura/escritura por rango. **Toda escritura pasa por un único `write()`, que aplica la lista de bloqueo y el recorte al rango del descriptor.** Ningún otro módulo escribe en el hidraw |
| `descriptor.rs` + `devices/pd100w.json` (embebido con `include_str!`) | Campos: `key` con punto (p. ej. `light.brightness`), `id`, `type` (`bool`, `int`, `enum`, `color`), `min`/`max`/`step`, `encoding` (`raw`, `db10_2000`, `x100`), opciones con clave de traducción, `group` y `status` (`verified`, `experimental`). Un descriptor externo en `~/.config/maono/devices/` reemplaza al embebido |
| `filter.rs` | Genera `~/.config/pipewire/filter-chain.conf.d/maono-clean.conf` desde el estado del filtro. Aplica parámetros en vivo con `pw-cli set-param <nodo> Props` (verificado). Si cambia la estructura (activar o desactivar una etapa), reescribe el conf y reinicia `filter-chain.service` |
| `profiles.rs` | Lee y escribe `~/.config/maono/profiles/<slug>.json`. Un perfil es **parcial**: solo aplica las claves que tiene. Arma un plan (diff contra el estado actual) para escribir el mínimo de campos |
| `presets/eq/*.json` (embebidos) | Las 7 curvas de Maono (Original, Game1/2, Stream1/2, Pop, Folk) como presets del EQ de PipeWire. Los presets del usuario van en `~/.config/maono/eq/` |
| `serve.rs` | Proceso de larga vida. Una sola conexión al mic, reconexión con backoff, protocolo JSONL (§4), medidor y reaplicación del perfil al conectar |
| `main.rs` | CLI: `status`, `get`, `set key=value…`, `schema`, `profile list/apply/save/rename/delete`, `filter …`, `serve`, `shell install/uninstall`. Se mantienen `scan` y el TUI |

**Configuración del backend:** `~/.config/maono/config.json` guarda `activeProfile`,
`applyOnConnect` (por defecto `true`, porque no se sabe si los ajustes sobreviven a un
apagado) y `filter.enabled`. Los perfiles de fábrica se copian la primera vez y después
son del usuario.

### 3.2 Cadena de PipeWire generada

```
mic crudo ─▶ HPF ×2 (bq_highpass) ─▶ EQ 5 bandas (bq_peaking / bq_lowshelf / bq_highshelf)
          ─▶ LPF (bq_lowpass) ─▶ RNNoise (VAD, gracia) ─▶ compresor LSP (si está instalado) ─▶ maono_clean
```

- Las etapas desactivadas no se generan.
- La captura usa `node.dont-fallback` y `node.linger`, como hoy. Así nunca toma otro mic.
- El nombre del nodo de captura sale del mic detectado: por cable `…PD100W_Mic_USB…`, y el receptor tendrá otro nombre.

### 3.3 Plugin (`shell/`, id `io.github.agusmoura.maono`)

| Archivo | Rol |
|---|---|
| `manifest.json` | `bar-widget`, `defaults` y `schema` declarados (Omarchy todavía no los dibuja, pero quedan listos) |
| `Service.qml` | Mantiene vivo `maono serve`, se reinicia con backoff y expone el estado y `send(cmd)`. Usa escritura optimista con `_pending`, igual que en el plugin de los Sony |
| `BarWidget.qml` | Icono 󰍬 / 󰍭 (mute en color urgente), punto de batería baja y acciones configurables (§5.3). Hostea `Loader { Panel.qml }` |
| `Panel.qml` | `KeyboardPanel` de 380 px, con alto máximo de 640 y scroll |
| `Model.js` | Funciones puras: textos, mapeos, validación de perfiles, curva del EQ. Se testea con `node --test` |
| `i18n/es.json`, `i18n/en.json` | Textos. Idioma: `settings.language`, o el prefijo de `Qt.locale().name`, o `en` |

Dónde vive cada configuración: las preferencias de la UI (acciones de la barra, idioma,
experimental, paso de la rueda) van en la entrada del widget en `shell.json`, escritas con
`bar.shell.updateEntryInline` y con un caché local para escrituras seguidas. El estado del
mic, los perfiles y el filtro van en `~/.config/maono/`, que es del backend.

## 4. Protocolo `maono serve` (JSON por línea)

**Comandos (stdin):**

```json
{"id":1,"cmd":"set","changes":{"mic.gain":14,"light.brightness":60}}
{"id":2,"cmd":"filter.set","changes":{"eq.bands.0.gain":3,"rnnoise.vad":80}}
{"id":3,"cmd":"profile.apply","name":"llamada"}
{"id":4,"cmd":"profile.save","name":"Mi voz","from":"current"}
{"id":5,"cmd":"profile.delete","name":"mi-voz"}
{"id":6,"cmd":"refresh"}
```

**Eventos (stdout):**

```json
{"ev":"hello","schema":{…descriptor + presets…},"profiles":[…],"config":{…}}
{"ev":"state","connected":true,"mic":{…},"filter":{…},"activeProfile":"llamada","dirty":false}
{"ev":"changed","key":"mic.mute","value":true,"source":"button"}
{"ev":"ack","id":1,"ok":true}   {"ev":"ack","id":3,"ok":false,"error":"…"}
{"ev":"level","value":0.42}          (máximo ~15 por segundo, solo con el panel abierto)
{"ev":"disconnected"}  {"ev":"error","code":"permission","hint":"udev"}
```

- Un perfil queda "sucio" (`dirty`) cuando el estado se aleja del perfil activo. El panel lo muestra con un punto y la opción "Guardar cambios".
- El medidor se activa con `{"cmd":"meter","on":true}`, que escribe 0x0045 = 0, y se apaga al cerrar el panel.

## 5. Panel

### 5.1 Estructura

**Cabecera (`PanelHero`):**

- Nombre, batería (con ⚡ si carga) y firmware en el detalle.
- Medidor de nivel de la fuente limpia, con `PwNodePeakMonitor` nativo; muestra lo que escuchan los demás.
- Interruptor de mute.
- Selector de perfil activo, con el punto de "sucio".

**Pestañas** (tira propia como en el plugin de los Sony; se cambian con `1`–`6`, o `h`/`l` sobre la tira):

| # | Pestaña | Contenido |
|---|---|---|
| 1 | Voz | Ganancia (slider que ignora la rueda), supresión del mic (off / baja / media / alta). Plegado: "Auriculares del mic", con monitoreo (voz / PC / ambos) y volumen 0–20 |
| 2 | EQ | Chips de presets (los de Maono y los propios), curva dibujada en Canvas, 5 bandas editables (tipo, frecuencia, ganancia, Q), pasa-altos y pasa-bajos, "Guardar como preset" |
| 3 | Ruido | RNNoise on/off, umbral VAD, gracia, pasa-altos anti-golpes (frecuencia), compresor (on/off, umbral, ratio, ataque, release; si falta LSP, muestra la pista de instalación), fuente por defecto limpia o cruda |
| 4 | Luz | On/off, efecto (fijo / respiración / ciclo), brillo, 8 colores y colores propios (hasta 5), con alta, edición HSV y borrado |
| 5 | Perfiles | Lista con aplicar, duplicar, renombrar y borrar (borrar pide confirmación en 2 pasos). "Guardar estado actual como…", interruptor "Reaplicar el perfil activo al conectar" y el comando para atajos (copiable) |
| 6 | ⚙ Ajustes | Acciones de la barra, paso de la rueda, ocultar si está desconectado, mostrar batería en la barra, idioma, "Mostrar controles experimentales del mic" e info del equipo (firmware, serie, ruta hidraw) |

Con "experimental" activado aparece, en Voz, la sección **"DSP del mic (no verificado)"**:
compresor, limitador, reverb y EQ interno, cada uno con la insignia "no verificado".

### 5.2 Convenciones de Omarchy

- `KeyboardPanel` + `PanelKeyCatcher`. Un solo cursor compartido por mouse y teclado; no se pinta hasta la primera tecla o el primer hover.
- Teclas: `j`/`k` filas, `h`/`l` ajustar, Enter/Espacio activar, Esc cerrar, Tab pasar al panel vecino.
- `Style.space()`, `Style.font.*`, `Color.*` y `bar.foreground`. El texto atenuado usa `Qt.darker(fg, 1.4)`; nada de colores literales salvo los swatches de la luz, que vienen del descriptor.
- Secciones con `PanelSectionHeader` en mayúsculas y separadas por `PanelSeparator`.
- Los sliders dentro del scroll ignoran la rueda.
- Las escrituras de los sliders van con debounce de 120 ms y escritura optimista. La UI nunca relee justo después de escribir; `serve` ya relee.
- Estados vacíos y de error dentro del panel: mic desconectado, falta la regla udev, filter-chain caído, falta LSP. Cada uno con una línea de ayuda accionable.
- Pie con atajos de teclado en `font.caption` y opacidad 0.4.

### 5.3 Barra e IPC

- **Acciones por defecto** (todas configurables en Ajustes y en `shell.json`): clic izquierdo abre el panel, clic medio mutea, clic derecho pasa al perfil siguiente, rueda cambia la ganancia ±1.
- **IPC** (`target: "maono"`, literal): `toggle`, `open`, `close`, `toggleMute`, `nextProfile`, `applyProfile(name)`. Sirve para atajos de Hyprland.

## 6. Perfiles de fábrica

Valores iniciales; quedan como archivos editables.

| Perfil | Mic | Filtro |
|---|---|---|
| Llamada | ganancia 20, NR baja | HPF 90 Hz, EQ "Original", RNNoise VAD 85 %, compresor suave (−24 dB, 3:1) |
| Streaming / Discord | ganancia 20, NR baja, luz on | HPF 90 Hz, EQ "Stream1", RNNoise VAD 80 %, compresor (−20 dB, 4:1) |
| Grabación | ganancia 18, NR off | HPF 70 Hz, EQ "Original", RNNoise VAD 0 (denoise sin compuerta), sin compresor |
| Gaming | ganancia 20, NR media | HPF 100 Hz, EQ "Game1", RNNoise VAD 92 %, compresor (−20 dB, 4:1) |

La luz forma parte del perfil solo si el perfil la incluye. Los de fábrica, salvo Streaming, no la tocan.

## 7. Errores y seguridad

- La lista de bloqueo y el recorte de rango viven en el backend (`mic.rs`), nunca solo en la UI.
- Los ids desconocidos no son escribibles ni por `maono set`. `set <id>` crudo queda detrás de `--raw` con una advertencia.
- Si `serve` muere, `Service.qml` lo reinicia (1 s, 2 s, 4 s… hasta 30 s), y la barra muestra el icono atenuado.
- Si `filter-chain.service` está caído, el panel ofrece "Iniciar".
- Si el conf generado es inválido, se valida antes de reemplazar el archivo y se conserva el anterior.
- Si un perfil está corrupto, se ignora con un aviso. Nunca se borra solo.

## 8. Pruebas

- **Rust (`cargo test`):** codificación y decodificación de tramas (simple, múltiple, rango), conversiones (`db10_2000`, `x100`), lista de bloqueo y recorte, merge y plan de perfiles, generación del conf (comparación con snapshot) y `serve` con un transporte falso (trait `Transport`).
- **JS (`node --test`):** `Model.js` (textos, curva del EQ, validación de perfiles, idioma).
- **Plugin:** `omarchy plugin validate`. Prueba de humo: `omarchy restart shell`, después `omarchy-shell maono open` y una captura con `grim` del panel.
- **Manual con el mic:** botones físicos que se reflejan en el panel, desenchufar y enchufar, aplicar cada perfil y escuchar.

## 9. Fuera de alcance (por ahora)

- Paquete AUR del fork.
- Cambio automático de perfil según la app abierta.
- Soporte del receptor inalámbrico más allá de detectarlo.
- Actualización de firmware.
- PD200W y otros modelos (el descriptor externo los deja posibles).
