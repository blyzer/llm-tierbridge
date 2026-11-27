# Pasarela propia de modelos: diseño (borrador, 2026-11-26)

Estado: **investigación**. No hay ADR: no se ha decidido construir el enrutador. Detalle y evidencia en
`sources.md` (fuentes, licencias, rutas, riesgos), `catalog-proof.md` (esquema, cifras, precedencia) y
`usage-and-language.md` (uso medido y criterios de lenguaje), en este mismo directorio.

## 0. Conclusión

1. **Todo como datos; ningún motor ajeno.** Las siete fuentes se consumen como datos con un adaptador
   pequeño cada una. CLIProxyAPI y OmniRoute siguen corriendo como **sidecars sin modificar**; no se forkea
   su motor. freellmapi solo aporta su registro (migraciones TS), también como datos.
2. **El valor inmediato está en el catálogo, no en el enrutador.** Las pasarelas rellenan límites por
   defecto (90 % de la caché de cliproxyapi con 272K/16K; 40 % de easycliproxy con 128K/32K) y el coste en
   vivo es desconocido en el 100 % de las filas. Un catálogo normalizado con procedencia arregla eso
   ya, sin enrutador nuevo.
3. **Lenguaje: Rust para el catálogo** (decidido el 2026-11-26), **sin depender de `crates/catalog`**: ese
   crate arrastra `omp-con` y `omp-core`, internos del workspace de oh-my-pi. El catálogo propio reutiliza
   sus nombres de campo (el esquema ya lo hace), no su código. El plugin de CLIProxyAPI (rebanada 6) va en
   Go porque su SDK es Go; el contrato entre ambos es el JSON Schema. La carga medida (pico 16–25 requests
   en vuelo, ≤7 proveedores/min) no justifica BEAM.
4. **Sin enrutador propio: la política va como plugin de CLIProxyAPI** (decisión del 2026-11-26). Lo core
   de freellmapi (ranking, cuotas por modelo y presupuesto) se implementa con `Scheduler` y `QuotaProvider`
   de la API de plugins, sin forkear el motor (`freellmapi-into-cpa.md` §5, `cpa-plugin-smoke.md`). El fork
   de CLIProxyAPI queda solo para lo que esa API no cubra. Línea base que el plugin debe mejorar: 39,7 % de
   fallos en CLIProxyAPI y 132/181 combos de OmniRoute con más de un intento. Hoy no se sustituye nada:
   el advisor usa `provider/advisor-model` y los fallbacks ya apuntan a `easycliproxy/*`.

## 1. Datos o motor (pregunta 1)

| Fuente | Licencia | Valor | Estrategia | Archivo de datos |
|---|---|---|---|---|
| models.dev | MIT | límites, coste de lista, modalidades, `canonical_model_id` | solo datos | `api.json` (10 049 modelos, refresco ~6 h) |
| modelparams.dev (antes modelparameters.dev) | MIT, robots permite | **parámetros aceptados**, enums de `reasoning_effort`, ciclo de vida | solo datos | API JSON estática (510 modelos) |
| CLIProxyAPI | MIT | qué sirve en vivo; thinking y límites de ruta | sidecar intacto + datos | `models.json` (212 modelos) + `/v1/models` vivo |
| router-for-me/models | **sin LICENSE** (consumir solo como dato factual, con atribución) | catálogo canónico de CLIProxyAPI: `models.json`, `devin_models.json`, `codex_client_models.json`, `native_capabilities` con semántica de tres estados + `native-capabilities-evidence.json`; registra retiradas (p. ej. GPT-6.1 Terra, 2026-11-25) | solo datos; fuente de "qué sirve CLIProxyAPI" y de ciclo de vida | `~/repositories/models` |
| CLIProxyAPI Management Center | MIT | panel administrativo del core | asset de release, servido por CLIProxyAPI | `~/repositories/Cli-Proxy-API-Management-Center` |
| Home Management Center | MIT | consola de **CLIProxyAPIHome** (multiusuario): credenciales upstream, tráfico, modelos disponibles, claves y uso por usuario | candidata para la pieza (d); evaluar si CLIProxyAPIHome sustituye al core en esta máquina | `~/repositories/Home-Management-Center` |
| EasyCLIProxyAPI | MIT | app de escritorio que hoy supervisa el binario | no adoptar | — |
| OmniRoute | MIT | `RegistryModel` con `unsupportedParams`, `supportedThinkingEfforts` | sidecar intacto + datos | registro del repo (`cheaper-inference/OmniRoute`) |
| freellmapi | MIT | registro de proveedores gratuitos | solo datos (adaptador sobre sus migraciones) | migraciones TS aditivas |
| dsh-our-free-model | MIT + apéndice | catálogo de canales gratuitos | solo datos | `catalog/`, `feed/` (local v2.2.0; remoto v2.9.1) |

Hipótesis inicial confirmada: models.dev y modelparams.dev como datos; el único motor que faltaría
(enrutado, fallback, balanceo) no se toma de nadie.

**Riesgo legal (decisión: no usar, solo leer catálogos):** dsh reproduce byte a byte el cuerpo del
cliente OpenCode para pasar el fingerprint del tier gratuito y trae pools de cuentas con captcha y huella
de máquina (`vendor/channel-pack`); OmniRoute documenta fingerprint TLS, MITM y proveedores por cookie;
CLIProxyAPI rota cuentas OAuth de suscripción de terceros; el feed vivo de freellmapi es de pago. El
adaptador de cada uno lee **su catálogo publicado**, nunca sus canales.

## 2. Normalización (pregunta 2)

- Esquema único `gateway-model.schema.json` (JSON Schema, prototipo en `/tmp/gateway-proof/schema/`), con
  los nombres de campo de `omp-catalog`. Dos registros: `observation` (una fuente, un modelo) y `model`
  (fusionado: valores ganadores + `references[]` + `provenance.fields`).
- Emparejamiento de ids en cuatro niveles (exacto, hoja exacta, normalizado, sin fecha). Resultado:
  1 264 de 1 762 ids de pasarela (71,7 %) casan con alguna referencia; ~la mitad de los 498 restantes son
  modelos no-chat (imagen, STT, embeddings).
- **Precedencia por campo** (tabla completa en `catalog-proof.md` §9). Resumen: identidad y modalidades del
  gateway vivo; límites del gateway **solo si no son el valor por defecto detectado**, si no models.dev de
  primera parte; coste nunca de un 0 del gateway; `acceptedParams` y ciclo de vida de modelparams.dev.
  Cada campo guarda ganador, regla aplicada, confianza y alternativas, así todo valor es auditable.
- La validación ya detectó datos rotos en origen (precio −1 000 000, presupuesto −1, esfuerzo `ultra`
  fuera de enum): el pipeline debe fallar en alto con esos casos, no normalizarlos en silencio.

## 3. Sincronización (pregunta 3)

Al ser todo "solo datos", basta un job determinista por fuente, sin LLM:

a. Descargar y anotar versión (commit/tag/etag + fecha); comparar con la última sincronizada (registro
   versionado `sync-state.json`).
b. Validar contra el esquema de la fuente. Si cambia el formato o el resultado queda vacío o encoge más
   de un umbral: **fallar en alto**, avisar y no escribir catálogo.
c. Clasificar el diff: modelo añadido, retirado, límite o coste cambiado, parámetro cambiado, fuera de
   alcance (listado aparte).
d. Traducir al esquema propio con el adaptador y abrir un PR revisable con el diff clasificado. Nada se
   aplica solo.
e. Tras fusionar, actualizar `sync-state.json`.

Excepciones explícitas (`overrides.json`, versionado): decisiones propias que ganan a cualquier fuente,
por ejemplo un parámetro quitado a propósito. Cadencias: models.dev diaria; modelparams.dev semanal;
CLIProxyAPI y dsh por release; OmniRoute semanal (≈4 270 commits/mes: no por commit). Una rama de
sincronizado está obsoleta si su versión de origen no es la última o lleva más de dos ciclos sin fusionar.

Si algún día se forkea un motor, el diseño ya separa datos de motor: catálogo, parámetros y capacidades
viven en su propia ruta con esquema propio, así que el fork no choca con el sincronizado.

## 4. Modularidad (pregunta 4)

| Pieza | Contrato | Implementación propuesta |
|---|---|---|
| (a) Adaptadores de fuente | entrada: dato crudo; salida: `observation[]` válido | uno por fuente; prototipo Python → Rust en la rebanada 3 |
| (b) Normalizador + catálogo | `observation[]` → `model[]` con procedencia | Rust (`crates/catalog` propio de este repo; nombres de campo de `omp-catalog`, sin depender de él) |
| (c) Selección + fallback | lee `model[]`; ranking, cuotas y presupuesto | plugin de CLIProxyAPI (`Scheduler`, `QuotaProvider`; `.dylib` c-shared). Traductores, pool de credenciales y fallback siguen en CLIProxyAPI sin modificar |
| (d) Panel / API admin | lee `model[]` y `sync-state.json` | Management Center sin modificar más una página del plugin vía `ManagementAPI` (servida en `/v0/resource/plugins/<id>/…`); Home Management Center si se adopta CLIProxyAPIHome |

Contratos por JSON Schema (es lo que ya usa el catálogo; no hay protobuf en ese camino).

## 5. Lenguaje

| Criterio | Medición | Rust | Go | Elixir/BEAM |
|---|---|---|---|---|
| Concurrencia real | pico 16 (omp) / 20–25 (OmniRoute) en vuelo; ≤7 proveedores/min | sobra | sobra | sobrado: su ventaja no se usa |
| Código existente | `omp-catalog` (26 k líneas) depende de crates internos de oh-my-pi | reutiliza sus nombres de campo, no el código | duplicaría | duplicaría |
| Runtime extra | Rust 1.99, Go 1.27.2, Elixir 1.20.5/OTP 29 instalados | ninguno | ninguno | runtime y release aparte |
| Empaquetado para slash command | binario único / servicio `o2ctl` | binario único | binario único | release OTP |
| Dentro o fuera de omp | catálogo en proceso; enrutador, si llega, como servicio | ambos | servicio | servicio |

Decisión (2026-11-26): **Rust** para catálogo y adaptadores; **Go** solo para el plugin de CLIProxyAPI, que su SDK exige.

Matiz: el omp de uso diario es el TS 19.2.1 (`packages/catalog`), no el omp en Rust. La salida práctica
de la primera fase es un **catálogo exportable** (overrides de `models.yml` o JSON que omp TS consuma),
generado por el binario Rust. Así sirve a los dos omp.

## 6. Plan por rebanadas

1. **models.dev → catálogo normalizado** (sin enrutador). Aceptación: el binario descarga `api.json`,
   valida y emite `model[]` con procedencia; reproduce las cifras de `catalog-proof.md` (±1 %); falla en
   alto con un `api.json` truncado (test).
   **Hecho (2026-11-26, `457fb9c`):** `tierbridge catalog build --source models.dev`; 10 049 observaciones
   idénticas a las del prototipo (test de paridad), todo válido contra el esquema; 2 valores rechazados
   (esfuerzo `default` en groq), 424 centinelas registrados por fila (422 límites publicados como 0,
   casi todos modelos de imagen, vídeo o audio, y 2 `min = -1` de presupuesto dinámico). Pendiente para la
   rebanada 3: `qwen/qwen3.6-27b` queda en `mode: effort` con solo `off` (¿debería ser `toggle`?), y los
   costes que el esquema no modela (`cost.reasoning`, audio) se descartan sin diagnóstico.
2. **modelparams.dev** → `acceptedParams` y ciclo de vida. Aceptación: 510 modelos ingeridos; los tres
   valores rotos detectados quedan rechazados con mensaje.
   **Hecho (2026-11-26, `6a0ce11`):** 510 modelos de 22 proveedores, idénticos al prototipo. De los tres
   valores rotos, solo uno viene de modelparams.dev: el precio −1 000 000 es de omp-bundled
   (`openrouter/auto`) y `ultra` no aparece; lo que hay es el esfuerzo `default` (5 rechazos). El
   presupuesto −1 está documentado por la propia fuente ("dinámico" o "sin límite"): se conserva como
   valor de wire con 10 centinelas registrados. Pendiente para la rebanada 3: 62 claves `provider/id`
   aparecen con `authType` `api_key` y `subscription`, y la fusión debe distinguirlas.
3. **Adaptadores de pasarela** (CLIProxyAPI `models.json` + `/v1/models`, OmniRoute, dsh, freellmapi) y
   merge con la tabla de precedencia. Aceptación: 0 filas con límites por defecto como ganadoras; coste
   `unknown` explícito en lugar de 0.
   **Hecho (2026-11-26, `3baf72a`):** 4 355 modelos de 6 pasarelas (caché de CLIProxyAPI, `omp models`
   easycliproxy y dsh, `router-for-me/models`, dsh y freellmapi), 75,5 % emparejados con alguna referencia.
   **0 límites de relleno ganadores**; coste conocido en 2 176, gratis en 342 (solo de referencias) y
   `unknown` explícito en 1 837, con 0 costes de pasarela a 0. Cada campo lleva regla, confianza,
   conflicto y alternativas. Decisiones con datos: `range.max` de modelparams solo cuando models.dev no
   tiene valor (coincide en el 57,5 % y difiere sin dirección clara); listas `{none, default}` →
   `toggle`; costes no modelados (razonamiento, audio) como diagnósticos por fila. Pendiente: OmniRoute
   (su registro es código TS), inferir `kind` de las salidas de models.dev, `catalog merge --in-dir` y
   acotar `references[]` (el catálogo ocupa 28 MB, ~6,8 KB por modelo).
4. **Pipeline de sincronizado** (a–e de §3) con PR clasificado y `overrides.json`. Aceptación: un cambio
   simulado de formato aguas arriba hace fallar el job sin tocar el catálogo.
   **Hecho (2026-11-27, `560da9a`):** `tierbridge sync init|run|apply|install-launchd`. Los datos viven en un
   repo privado aparte (índice compacto por fuente, `sync-state.json`, `overrides/omp.yml`, informes); cada
   cambio abre un PR con el diff clasificado (añadido, retirado, límite, coste, ciclo de vida, esfuerzos,
   fuera de alcance). Un cambio de formato o una caída de más del 20 % hace fallar el job sin tocar el
   repo de datos y avisa con una notificación de macOS. `sync apply` es manual. Job diario de launchd a
   las 06:00; las claves salen del Llavero (`security … -s llm-tierbridge`). Pendiente: cerrar
   automáticamente ramas de sincronizado obsoletas y avisar también de los éxitos.
5. **Integración**: generar desde el catálogo el bloque `openai-compatibility` de CLIProxyAPI y exportar
   overrides para `easycliproxy`/`dsh`/`omni` en `models.yml` (límites y coste reales). Aceptación:
   CLIProxyAPI recarga la config generada y `/v1/models` lista sus modelos; `omp models easycliproxy --json`
   deja de mostrar 128K/32K en los modelos corregidos; el advisor y los fallbacks siguen funcionando.
   **Hecho (2026-11-27, `04b9ee9`):** `export omp` + `apply omp` (bloque marcado en `models.yml`, copia de
   seguridad, `--revert`, idempotente) + `snapshot omp-live` (lee omp sin los bloques gestionados, sin
   realimentación). **Aplicado al `~/.omp/agent/models.yml` real** con referencias descargadas el
   2026-11-27: easycliproxy 1 699 modelos, contexto 128K 712 → 490, salida 32K 734 → 542, precio
   desconocido 1 699 → 911; dsh 20/21/51 → 5/7/44. Los 472 que conservan relleno no tienen evidencia
   (376 sin referencia emparejada, 77 sin límite publicado, 19 con referencias en desacuerdo), entre
   ellos los enrutadores `auto/*`. Fallbacks resueltos y una petición real por
   `easycliproxy/agnes-3.0-flash` responde. `export cpa` genera el bloque para
   `openai-compatibility` (probado con un CLIProxyAPI temporal en otro puerto: 1 663 alias listados y
   una petición servida); la instancia en vivo no tiene proveedores OpenAI-compatibles, así que no se
   aplicó. Pendiente: refresco periódico (rebanada 4), modelos nuevos sin override hasta reexportar,
   tramos de precio y `thinking` no exportados.
6. **Plugin de CLIProxyAPI**: `Scheduler` + `QuotaProvider` + página de `ManagementAPI`, con la versión de
   CLIProxyAPI fijada (la ABI cambia a menudo). Aceptación: el plugin carga y registra las tres
   capacidades; el orden de la cadena sigue el ranking del catálogo; un modelo sin cuota se salta; se
   comprueba qué hace CLIProxyAPI cuando el plugin devuelve error; la tasa de fallos medida baja frente a
   la línea base de §0.
7. **(Condicional) `ProviderExecutor` o fork mínimo**: `ProviderExecutor` solo para los proveedores nativos
   no OpenAI que se usen de verdad. Fork de CLIProxyAPI solo si la API de plugins no basta; requiere ADR y
   el parche se propone también upstream.

## 7. Pendiente / sin verificar

ToS de Anthropic, OpenAI, Google, OpenCode y Kilo; términos del feed Premium de freellmapi; apéndice
completo de licencias de dsh; `merge.py` de la prueba (precedencia aún no ejecutada); comparación directa
models.dev ↔ modelparams.dev.
