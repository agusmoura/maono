Revisión sobre omarchy-panel, commit 354118e9722c49eb43448368e9a59fe84edeca50. El repositorio sigue limpio. No ejecuté maono, accedí a hidraw ni reinicié servicios. Las afirmaciones de calibración del spec se toman como antecedentes, no como verificaciones realizadas en esta revisión.

Referencias abreviadas:
  Spec: docs/superpowers/specs/2026-10-07-omarchy-panel-design.md
  Protocolo: docs/protocol/pd100w.md
  Sony: ~/.config/omarchy/plugins/io.github.agusmoura.sony-headphones/
  Omarchy: /usr/share/omarchy/shell/
  Conf actual: ~/.config/pipewire/filter-chain.conf.d/maono-clean.conf

CRÍTICO

1. “Una sola conexión” no resuelve la concurrencia de CLI y TUI.
   Problema: el spec conserva escritores externos y confía en recibir sus respuestas GET (§2, §3). Que esas respuestas lleguen a todos no serializa operaciones ni evita respuestas viejas, perfiles entremezclados o incrementos perdidos.
   Propuesta: un único dueño del hidraw y del estado; CLI/TUI envían comandos a ese dueño mediante un socket Unix sencillo. Si se admite acceso directo sin serve, usar el mismo lock y rechazarlo mientras el dueño esté activo.

2. Service.qml no queda singleton siguiendo literalmente el patrón Sony.
   Problema: el manifest propuesto declara solo bar-widget (§3.3). Sony/BarWidget.qml:43–46 instancia un Service por widget; Sony/Service.qml:6–8 reconoce la duplicación. Omarchy crea barras por pantalla (plugins/bar/Bar.qml:1196–1204).
   Por qué importa: dos monitores pueden producir dos serve y dos reaplicaciones del perfil.
   Propuesta: declarar kinds service + bar-widget y entryPoints.service; obtener el Service mediante serviceFor. Omarchy ya implementa esa instancia compartida (shell.qml:901–950).

3. La barrera de seguridad puede quedar anulada por --raw o un descriptor externo.
   Problema: §3.1 permite reemplazar el descriptor; §7 prohíbe IDs desconocidos, pero después admite set crudo con advertencia. Una lista de bloqueo no cubre todos los comandos peligrosos desconocidos.
   Por qué importa: el firmware no valida valores (src/mic.rs:15–17); el reset está identificado en Protocolo:278.
   Propuesta: allowlist inmutable de IDs, permisos y límites de seguridad; el descriptor externo no puede ampliarla. Validar cada campo de SET simple, múltiple y rango antes de enviar nada. --raw no debe saltarse esa barrera.

4. Aplicar un perfil no tiene contrato ante un fallo intermedio.
   Problema: el plan de diff (§3.1) combina escrituras HID, parámetros PipeWire, archivos y posibles reinicios, sin definir cuándo se confirma activeProfile ni qué ocurre si falla la mitad.
   Por qué importa: puede quedar el mic nuevo con el filtro viejo y mostrarse éxito.
   Propuesta: validar todo antes de actuar, serializar la aplicación y confirmar cada parte. Actualizar activeProfile solo al completar; ante fallo, informar qué quedó aplicado y conservar un estado parcial explícito. Restaurar el filtro anterior cuando sea posible, sin prometer atomicidad del hardware.

5. Reaplicar por defecto puede revertir una decisión de privacidad.
   Problema: applyOnConnect=true (§3.1) no distingue reenchufado físico, reinicio de serve y hot-reload. Un perfil guardado con mute=false podría desmutear; además puede borrar ajustes recientes todavía “sucios”.
   Propuesta: no desmutear automáticamente. Separar sincronización inicial de reconexión física y definir qué se conserva cuando dirty=true. Primera instalación: leer el estado, no aplicar silenciosamente un perfil de fábrica.

IMPORTANTE

6. Perfiles parciales contradicen “deja todo en el mismo estado”.
   Problema: §1 exige igualdad entre panel, CLI y atajo; §3.1 aplica solo claves presentes y §6 deja la luz sin tocar en varios perfiles. El resultado depende del estado previo.
   Propuesta: mantener perfiles parciales, pero prometer igualdad únicamente en las claves incluidas. Calcular dirty sobre esas claves, con valores normalizados; no sobre batería, serie, medidor ni preferencias de UI.

7. Guardar y administrar perfiles necesita reglas de datos.
   Problema: §4 y §5.1 no definen formato versionado, colisiones de slug, sobrescritura, rename/delete del activo ni si guardar incluye valores optimistas todavía pendientes.
   Por qué importa: se pueden perder perfiles o guardar un estado que nunca llegó al dispositivo.
   Propuesta: ID estable separado del nombre visible, slugs restringidos, escritura atómica y rechazo de colisiones. Guardar estado confirmado; no borrar/renombrar el activo sin actualizar su referencia. Las claves omitidas deben seguir omitidas salvo decisión explícita.

8. “Experimental” no sustituye límites seguros conocidos.
   Problema: el descriptor exige min/max (§3.1), pero el límite superior del EQ interno y algunos significados siguen abiertos (Protocolo:292–304). Clamp tampoco valida tipos, enums, pasos o referencias entre campos.
   Propuesta: rechazar entradas inválidas antes de escribir y devolver el valor efectivo. Los campos sin un rango justificable quedan read-only hasta documentarlo; los demás pueden seguir accesibles como experimentales.

9. La lectura actual pierde eventos y oculta desconexiones.
   Problema: try_read convierte todos los errores en None (src/mic.rs:190–196); get descarta notificaciones mientras espera (210–224); parse procesa solo la primera trama (130–156), aunque el protocolo permite concatenación (Protocolo:45).
   Por qué importa: reutilizar ese flujo rompe la actualización de botones en menos de un segundo y la reconexión.
   Propuesta: un lector que distribuya todas las tramas a estado y peticiones pendientes; distinguir WouldBlock, EOF y errores reales. Evitar sleeps bloqueantes en el loop de serve.

10. Detección no equivale a selección segura del dispositivo.
    Problema: find_device devuelve el primer nodo ordenado que coincide con receptor o cable (src/mic.rs:83–98). El receptor tiene otro mapa y está fuera de alcance (§9; Protocolo §4.7).
    Propuesta: seleccionar explícitamente el PD100W cableado y confirmar identidad antes de reaplicar. Si aparecen varios candidatos, no elegir arbitrariamente. Detectar el receptor debe mostrar “detectado, control no soportado”, no habilitarle el descriptor cableado.

11. JSONL todavía no es un contrato implementable completo.
    Problema: §4 no define versión, orden inicial, framing estricto, significado de ack, timeout ni actualización de profiles/config después de CRUD. Los ejemplos incluso ponen varios JSON en una misma línea.
    Propuesta: una línea por mensaje, stdout exclusivamente JSONL y logs en stderr; hello versionado seguido de snapshot. ACK con éxito solo tras confirmación, incluyendo valores efectivos o seguido de estado completo. Añadir los comandos de configuración, rename/duplicate y presets que requiere la UI.

12. Copiar _pending del Sony puede perder cambios.
    Problema: Sony/Service.qml:17–19 y 110–120 conserva un único campo pendiente y una única petición en cola. Cambiar ganancia y luego luz puede reemplazar una acción distinta.
    Propuesta: pending por clave e ID de petición; coalescer solo cambios sucesivos de la misma clave. Un ACK viejo no debe borrar una edición más nueva. En error o desconexión, retirar el optimismo y mostrar el último valor confirmado.

13. El hot-reload tiene efectos sobre hardware no definidos.
    Problema: Omarchy destruye servicios no keepLoaded y widgets durante reload (shell.qml:1033–1043, 1452–1461). Reiniciar serve puede disparar applyOnConnect y perder órdenes pendientes.
    Propuesta: definir shutdown por EOF, liberación del dueño y sincronización sin reaplicar al arrancar. keepLoaded puede evitar ese churn si se acepta que los cambios del Service requieren reiniciar shell; no usarlo como sustituto del lock del backend.

14. Hay dos medidores para una sola necesidad.
    Problema: §4 transmite nivel HID y escribe 0x0045 al abrir/cerrar; §5.1 usa PwNodePeakMonitor sobre la fuente limpia. Miden señales distintas y el control HID afecta globalmente al stream del dispositivo.
    Propuesta: eliminar meter/level HID del panel y usar el monitor nativo, como Omarchy/plugins/panels/audio/Panel.qml:580–584. Etiquetarlo como nivel de la fuente seleccionada: no garantiza lo que recibe una app que usa otra entrada.

15. Los cambios vivos del filtro pueden desaparecer al reiniciar.
    Problema: §3.1 reescribe el conf solo ante cambios estructurales. pw-cli set-param modifica el grafo en memoria, no el archivo.
    Propuesta: persistir también los parámetros aceptados, sin reiniciar por ello. Definir una única fuente de verdad y cómo se recuperan los cambios actuales aunque todavía no estén guardados como perfil.

16. Reiniciar filter-chain tiene alcance global y corta audio.
    Problema: §3.1 reinicia el servicio al activar/desactivar etapas. La unidad ejecuta pipewire -c filter-chain.conf (/usr/lib/systemd/user/filter-chain.service:15), que carga todos los fragmentos, no solo Maono.
    Propuesta: agrupar los cambios estructurales de un perfil en un único reinicio y advertir el corte. Preferir bypass en vivo donde el plugin lo permita. Un servicio separado solo se justifica si realmente hay que aislar otros filtros.

17. “Validar el conf” no basta para recuperar un fallo.
    Problema: §7 no distingue sintaxis de disponibilidad de plugins, labels, puertos y creación del nodo. El conf actual usa nofail (Conf actual:8), por lo que el servicio puede seguir activo sin maono_clean.
    Propuesta: validar datos y dependencias antes del reemplazo, conservar el archivo anterior y comprobar nodo/Props después de cargar. Si falla, restaurar el anterior y reportarlo. No devolver éxito basándose únicamente en systemd.

18. Falta el contrato técnico del DSP de software.
    Problema: §3.2 no fija mono/48 kHz ni mapeos concretos de puertos y unidades. El conf actual sí fija ambos (24–25). LSP usa amplitud lineal para Attack threshold, no el encoding HID de dB; RNNoise LADSPA tiene VAD 0–99 y dos gracias distintas.
    Propuesta: datos explícitos para plugin/label, controles, unidades y conversiones; conservar mono/48 kHz. Exponer gracia normal y retroactiva —esta última añade latencia— sin copiar rangos del mic al software.

19. Compresor opcional y perfil reproducible requieren una decisión.
    Problema: §3.2 omite LSP si no está instalado, pero §6 lo exige en varios perfiles.
    Propuesta: rechazar la aplicación antes de tocar el mic si falta una dependencia requerida, o pedir/aplicar una variante explícita sin compresión. No omitirlo silenciosamente y marcar el perfil como plenamente aplicado. Cubrir también RNNoise ausente.

20. HPF tiene dos ubicaciones y precedencia ambigua.
    Problema: aparece en EQ y Ruido (§5.1); los presets tienen HPF/LPF propios (Protocolo:253–261), y los perfiles fijan otro HPF (§6). La cadena agrega además HPF ×2.
    Propuesta: un único estado de pasa-altos, una única ubicación de edición y pendiente documentada. Aplicar primero el preset y después los overrides del perfil. Aclarar si el ×2 constituye ese mismo HPF o una etapa adicional.

21. El generador no cubre todos los cambios estructurales.
    Problema: cambiar peak por shelf modifica el label, no solo un control; apagar todas las etapas deja un grafo vacío (§3.1–§3.2).
    Propuesta: clasificar cambios de tipo como estructurales y usar builtin copy para la cadena totalmente desactivada, manteniendo la fuente virtual estable. No incorporar un motor genérico de grafos para este conjunto fijo.

22. El nodo PipeWire no se deduce del nombre del hidraw.
    Problema: §3.2 dice que sale del mic detectado, pero no define cómo correlacionarlo. El target actual contiene una identidad USB concreta (Conf actual:28), mientras la serie HID tiene incluso prefijo RX (Protocolo:217).
    Propuesta: descubrir Audio/Source por propiedades USB/dispositivo y resolver su node.name actual. No construirlo por concatenación ni asumir igualdad entre series HID y ALSA; no tomar otra fuente como fallback.

23. La fuente por defecto está declarada, pero no diseñada.
    Problema: §1 y §5.1 ofrecen limpia/cruda; no hay comando JSONL, persistencia ni política cuando el filtro desaparece. Crear Audio/Source no la selecciona automáticamente.
    Propuesta: usar la selección nativa de WirePlumber y verificarla. Persistir la preferencia; definir desconexión/desactivación y restauración. Aclarar que no cambia entradas fijadas explícitamente por Discord u otra app.

24. El monitor de auriculares no cubre el estado real completo.
    Problema: la UI ofrece voz/PC/ambos (§5.1), pero el protocolo incluye ninguno y distingue “ambos”=4 de all=7 (Protocolo:77–83). Volumen y monitor siguen sin confirmación funcional en los antecedentes aportados.
    Propuesta: incluir ninguno y representar sin falsear los valores leídos. Marcar estos controles como experimentales hasta verificarlos; usar el enum documentado, nunca un bitmask.

25. Los colores propios necesitan una operación compuesta.
    Problema: §5.1 propone altas/bajas, pero el descriptor de campos independientes no define orden entre HSV, cantidad y selección. Borrar un slot desplaza referencias de perfiles.
    Propuesta: una operación backend que valide todo, escriba datos/cantidad/selección en orden y confirme el resultado. Definir qué ocurre con perfiles que apuntaban al color borrado; en efecto ciclo, deshabilitar la selección de color (Protocolo:156–159).

26. updateEntryInline reemplaza la entrada, no aplica un patch.
    Problema: §3.3 menciona caché local, pero Omarchy/shell.qml:1092–1095 reconstruye la entrada con las claves recibidas. Un payload parcial borra preferencias; un caché viejo puede pisar cambios hechos desde otro monitor.
    Propuesta: enviar la entrada completa, fusionada con settings actuales y los cambios locales pendientes. Usar el ID del plugin, no el target IPC maono. Refrescar el caché con cambios externos; no crear un segundo escritor de shell.json en Rust.

27. Falta la migración y distribución del nuevo plugin.
    Problema: cambia maono a io.github.agusmoura.maono, pero el instalador actual usa el ID antiguo y copia solo dos archivos (src/shell.rs:13–17, 77–84). No copiaría Service, Model.js ni i18n.
    Propuesta: definir actualización sin dos widgets/targets activos, preservar settings y distribuir todos los assets. Instalar primero un conjunto completo validado y publicarlo después para evitar hot-reload de una copia a medio hacer. No hace falta crear un instalador genérico.

28. Seis pestañas son compatibles; copiarlas sin adaptación no lo es.
    Problema: KeyboardPanel y el patrón de pestañas Sony encajan con Omarchy. Pero Sony usa cuatro pestañas a 420 escalados (Sony/Panel.qml:40–45, 405–406); seis a 380, con traducciones, necesitan resolver ancho y legibilidad.
    Propuesta: mantenerlas si caben, con etiquetas breves y layout adaptable. Usar Style.space y fittedContentWidth/Height como el panel monitor. Compresor pertenece a dinámica/procesamiento, no claramente a “Ruido”; plegar los parámetros avanzados del EQ para no convertir el popup en una consola permanente.

29. El teclado no cubre los nuevos editores.
    Problema: renombrar perfiles y editar frecuencia/Q requiere entrada textual. PanelKeyCatcher intercepta navegación y caracteres salvo blocked (Omarchy/Ui/PanelKeyCatcher.qml:23–36, 48–82).
    Propuesta: bloquear atajos mientras haya un editor activo; definir Enter/Esc para confirmar/cancelar. Cubrir mute y selector de perfil de la cabecera, botones de cada fila, auto-scroll y reajuste del cursor al plegar, borrar o cambiar de pestaña.

30. Falta una forma rápida de comparar y volver a un estado conocido.
    Problema: hay edición extensa, pero no se especifican bypass global/EQ ni “reaplicar perfil” para descartar cambios. NR del mic más RNNoise y boosts de EQ pueden empeorar la voz sin que el usuario ubique la causa.
    Propuesta: hacer visible filter.enabled, añadir bypass de EQ y reaplicar el perfil activo. Mostrar claramente qué procesamiento está activo y advertir clipping; no sumar una herramienta de grabación ni un mezclador completo.

31. La aceptación prueba componentes, no los riesgos principales.
    Problema: §8 cubre frames, snapshots y humo, pero no escritores concurrentes, ACK atrasados, fallo parcial de perfil, reload, dependencia ausente ni recuperación del conf.
    Propuesta: añadir esos escenarios al transporte falso y al contrato Service/backend. La validación del manifest y una captura no prueban foco, scroll ni estado aplicado. Las pruebas reales de escritura deben quedar como una etapa separada y expresamente autorizada.

MENOR

32. La evidencia de calibración quedó dividida entre documentos.
    Problema: §2 afirma confirmaciones por SET, mientras Protocolo:3 y 61 declara investigación GET-only y conserva preguntas que el spec da por resueltas.
    Propuesta: añadir al mapa un suplemento de calibración con método y resultados. No borrar la investigación original ni repetir escrituras para justificar este documento.

33. “Nada hardcodeado” no requiere soporte extensible de modelos.
    Problema: §3.1 añade descriptors externos y §9 los justifica por modelos fuera de alcance.
    Propuesta: conservar el descriptor JSON embebido —cumple UI data-driven— y recortar overrides externos en esta entrega. Añadirlos cuando exista un segundo dispositivo validado. Mantener claves e IDs estables; traducir etiquetas, no identificadores.

34. Los estados desconectados e incompletos necesitan seguir siendo útiles.
    Problema: §5.1 permite ocultar el widget y §5.2 enumera errores, pero no define si Ajustes/Perfiles siguen accesibles ni cómo representar batería o valores todavía desconocidos.
    Propuesta: mantener acceso por IPC a diagnóstico y gestión offline; controles HID deshabilitados, no desaparecidos arbitrariamente. Diferenciar desconocido de 0, conservar valores recibidos fuera del rango de escritura sin “corregirlos” automáticamente y no mostrar un falso estado desmuteado.

aprobar con cambios

