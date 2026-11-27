# llm-tierbridge

Pasarela de modelos que une varios componentes sin forkearlos:

- **Motor:** [CLIProxyAPI](https://github.com/router-for-me/CLIProxyAPI) (MIT) sin modificar: traductores, pool de credenciales, thinking y API de gestión.
- **Política de selección:** las funciones core de [freellmapi](https://github.com/tashfeenahmed/freellmapi) (MIT), es decir ranking, cuotas por modelo y presupuesto, como **plugin** de CLIProxyAPI (`Scheduler`, `QuotaProvider`, `ProviderExecutor`, `ManagementAPI`).
- **Catálogo normalizado:** [models.dev](https://github.com/anomalyco/models.dev) (MIT), [modelparams.dev](https://github.com/mnfst/modelparams.dev) (MIT) y [router-for-me/models](https://github.com/router-for-me/models), este último sin licencia y usado solo como dato. Genera el bloque `openai-compatibility` de CLIProxyAPI.
- **Front:** [Cli-Proxy-API-Management-Center](https://github.com/router-for-me/Cli-Proxy-API-Management-Center) sin modificar, con una página propia del plugin.
- **Cliente omp:** proveedor `@blyzer/llm-tierbridge` para omp, sobre el adaptador de ChadCSong (MIT).

Paquetes npm bajo el scope `@blyzer`.

## Estado

Fase de diseño; la rebanada 1 (catálogo de models.dev) ya está implementada, ver «Rebanada 1». Decisiones del 2026-11-26:

1. Primero el plugin. El fork de CLIProxyAPI queda solo para lo que la API de plugins no cubra, como parche mínimo propuesto también upstream.
2. freellmapi se usa solo como datos (no se forkea). Su feed Premium no se consume.
3. El primer paso es común a ambos caminos: normalizador del catálogo y generador de configuración, que cubren unos 73 de los 104 proveedores de freellmapi sin código en CLIProxyAPI.
4. Comprobado: el CLIProxyAPI local (v8.1.7, compilado con cgo) carga plugins. Falta un plugin propio que declare `Scheduler` y `QuotaProvider` (`docs/cpa-plugin-smoke.md`).

## Rebanada 1: catálogo desde models.dev

Espacio de trabajo Cargo (Rust, edición 2024): `crates/catalog` (biblioteca `tierbridge_catalog`) y `crates/cli` (binario `tierbridge`). El esquema canónico está en `schema/gateway-model.schema.json`. `prototype/` conserva el prototipo en Python, que es la especificación de comportamiento.

```sh
cargo test                                    # incluye la paridad con las 10 049 observaciones del prototipo
cargo clippy --all-targets -- -D warnings
cargo fmt --check

cargo run --release -p tierbridge-cli -- catalog build \
  --source models.dev --input prototype/raw/models.dev.api.json --out-dir /tmp/tb-out
# varias fuentes: un --input por cada --source, emparejados por orden
cargo run --release -p tierbridge-cli -- catalog build \
  --source models.dev --input prototype/raw/models.dev.api.json \
  --source modelparams.dev --input prototype/raw/modelparams.models.json --out-dir /tmp/tb-out
# o descargando: --fetch en lugar de --input
```

Opciones de `tierbridge catalog build`:

| Opción | Efecto |
|---|---|
| `--source <id>` | `models.dev`, `modelparams.dev` o una fuente de la rebanada 3b (`router-for-me.models`, `dsh`, `freellmapi`, `omniroute`); repetible (no dos veces la misma) |
| `--input <archivo>` / `--fetch` | Una copia guardada por cada `--source`, emparejadas por posición, o descargar todas (`ureq`; los tests nunca usan red) |
| `--out-dir <dir>` | Carpeta de salida |
| `--strict` | Cualquier valor rechazado hace fallar la ejecución (código 3, no escribe nada) |
| `--min-models <N>` / `<fuente>=<N>` | Falla si una fuente trae menos de N modelos; `N` suelto vale para todas, `fuente=N` (repetible) manda sobre él |
| `--fetched-at <RFC 3339>` | Momento de descarga de los `--input` (el mismo para todos); por defecto, la fecha de modificación de cada archivo |

Qué emite, en `--out-dir`, **por cada fuente** (`<id>` = `models.dev` o `modelparams.dev`). Las fuentes no se fusionan (eso es la rebanada 3): hay un juego de tres archivos por fuente y `catalog.json` queda reservado para el catálogo fusionado. Si una fuente falla, no se escribe ningún archivo de ninguna.

- `observations.<id>.jsonl`: una observación por modelo, ordenadas por (proveedor, id) (y `authType` en modelparams.dev).
- `catalog.<id>.json`: `model[]` de una sola fuente; `key = proveedor/id`, procedencia por campo no nulo con regla `single-source` y confianza `declared`.
- `report.<id>.json`: recuentos, rechazos, centinelas mapeados (uno por fila en `sentinels`), y la revisión de la fuente (sha256, `fetchedAt`, y `ETag`/`Last-Modified` si se descargó). `counts.sentinels` es el total de centinelas (= `sentinels.length`), `counts.sentinelsByField` lo desglosa por campo y `counts.zeroLimitsMappedToUnknown` es la parte que son límites 0. En la instantánea del 2026-11-26: 424 centinelas = 2 `thinking.minBudgetTokens` + 155 `contextWindow` + 4 `maxInputTokens` + 263 `maxTokens`.

Tratamiento de valores fuera del contrato (nunca se descartan en silencio):

- **Error estructural** (JSON inválido o truncado, raíz que no es un objeto de proveedores, proveedor sin `models`, cero modelos, `--min-models` no alcanzado; en modelparams.dev además: sin `generatedAt` válido, sin `models` array, `count` distinto del número de modelos, entrada sin `provider`/`model`/`authType`/`apiSurface`/`params`, o el mismo `(provider, model, authType)` repetido): falla con código 1 y no toca ningún archivo de salida (se escribe a temporales y se renombra al final).
- **Valor rechazado** (esfuerzo fuera del enum, precio negativo, límite negativo o no entero, etc.): se quita del registro y queda en `report.<id>.json` como `{sourceId, provider, id, field, value, reason}`.
- **Centinela documentado** (se mapea, no es un rechazo, y `--strict` no lo afecta; cada uno queda en `report.<id>.json` como `{sourceId, provider, id, field, value, mappedTo, reason}`): `budget_tokens.min == -1` pasa a `dynamicBudget: true`; un límite igual a 0 pasa a desconocido (`mappedTo: "unknown"`, motivo «models.dev publishes 0 when no token limit applies»). El resumen por pantalla muestra solo el recuento por campo.

## Rebanada 2: modelparams.dev → `acceptedParams` y ciclo de vida

`crates/catalog/src/modelparams.rs` porta `adapt_modelparams()` del prototipo (`GET https://modelparams.dev/api/v1/models.json`, instantánea `generatedAt` 2026-11-24T08:22:46Z): `acceptedParams` (parámetros del cuerpo de la petición con tipo, grupo, valores, rango, valor por defecto y `conditional`), `lifecycle` (`active`→`live`, `deprecated`, `retired`; sin estado → `unknown`, con `replacement` y `shutdownOn`), `thinking` (niveles de esfuerzo de los enums del grupo `reasoning` cuya ruta contiene `effort`), `reasoning: true` solo si hay parámetros de ese grupo (su ausencia no prueba que el modelo no razone), `maxTokens` (el `range.max` positivo del último parámetro de longitud de salida) y `authType`. No trae precios, límites de contexto ni modalidades: esos campos quedan desconocidos. Fuente: `kind: bundled`, `confidence: declared`, como en el prototipo.

En la instantánea del 2026-11-26: **510 modelos** ingeridos (22 proveedores; el índice de la API también dice 510), idénticos fila a fila a `prototype/out/obs.modelparams.dev.jsonl` (test de paridad con fixtures zstd; un `null` equivale a ausente). 87 son contratos `subscription`; 62 de ellos comparten `provider/id` con su contrato `api_key`, así que `catalog.modelparams.dev.json` repite 62 claves (se distinguen por `authType`; fusionarlas es la rebanada 3).

Los «tres valores rotos» del diseño, en esta fuente:

| Valor del diseño | ¿Está en modelparams.dev? | Tratamiento |
|---|---|---|
| precio −1 000 000 | **No.** Esta fuente no publica precios; es de `omp-bundled` (`openrouter/auto`) y no se inventa aquí | — |
| esfuerzo `ultra` | **No.** El valor fuera del enum que sí aparece es `default` (`reasoning_effort` de 5 modelos de groq) | Rechazo por modelo en `thinking.efforts` (5); `acceptedParams` conserva la lista tal cual la publica la fuente |
| presupuesto −1 | **Sí**, 10 valores en 8 modelos: `thinkingBudget` de Gemini (`range.min` y `default`, 2 modelos) y `reasoning_budget` de NVIDIA (`range.min`, 6 modelos) | **Centinela documentado**, no rechazo: la descripción de la propia fuente dice «-1 uses dynamic thinking» / «Use -1 to disable budget enforcement». Es el valor que se envía por el cable, así que se conserva en `acceptedParams` y se anota un `SentinelMapping` (`mappedTo: "kept as the wire value"`, 10 en total) |

Regla general: en parámetros que cuentan tokens (grupos `reasoning` y `generation_length`) un límite o valor por defecto negativo se rechaza y se quita, salvo `-1` cuando la descripción del propio parámetro lo menciona. Los sampling pueden ser negativos (`frequency_penalty` desde −2) y no se tocan. Con `--strict` fallan los 5 rechazos y no los 10 centinelas.

## Rebanada 3b: fuentes de pasarela leídas de un repositorio

`router-for-me.models`, `dsh`, `freellmapi` y `omniroute` no tienen una URL con un solo archivo. Un script de `tools/` lee la copia local del repositorio **en un commit fijo** (sin red, sin tocar el árbol de trabajo) y escribe un *snapshot*: un JSON `tierbridge-snapshot/1` con `sourceId`, `repository`, `commit` (sha de 40 hex), `commitDate` (UTC) y la carga de la fuente. El adaptador valida esa cabecera (un error estructural no escribe nada) y fija `revision = "<sha>@<fecha>"`. `--fetch` se rechaza para estas fuentes (código 1, nada escrito): se pasa el snapshot con `--input`.

Reglas de las rebanadas 1 y 2: error estructural, falla; valor fuera de contrato, se quita y queda un `Diagnostic` por fila; centinela documentado, un `SentinelMapping`. Los datos que el esquema no puede representar van a `raw`.

### `router-for-me.models` (lo que sirve CLIProxyAPI)

```sh
node tools/snapshot-git.mjs router-for-me.models --repo ~/repositories/models --out /tmp/rfm.json
cargo run --release -p tierbridge-cli -- catalog build --source router-for-me.models --input /tmp/rfm.json --out-dir /tmp/tb-out
```

Sin LICENSE en origen: solo datos factuales y la advertencia va en `origin` del descriptor. Fixture: commit `d02fed8` (2026-11-25), 272 modelos servidos (212 de `models.json` + 60 de `devin_models.json`) y 248 retirados.

| Campo del origen | Campo de la observación |
|---|---|
| sección de `models.json` / `devin` | `provider` (el canal; un mismo id aparece en varios canales, p. ej. `codex-free` y `codex-pro`) |
| `id`, `display_name` | `id`, `name` (`name` del origen, p. ej. `models/gemini-2.5-pro`, va a `raw.resourceName`) |
| `context_length` / `inputTokenLimit` | `contextWindow` = `context_length`, o `inputTokenLimit` si es lo único que hay (Gemini). Si hay ambos (Copilot), `inputTokenLimit` es `maxInputTokens` |
| `max_completion_tokens` / `outputTokenLimit` | `maxTokens` |
| `supportedInput/OutputModalities` | `input`, `output`; `kind` = `image` si solo sale imagen, `chat` si sale texto, si no `null` |
| `thinking.levels` | `thinking.efforts` (`none` → `off`, y entonces `canDisable = true`); un nivel fuera del enum (`priority`, `auto`, `ultra`) es un rechazo por fila en `thinking.efforts` |
| `thinking.min/max/zero_allowed/dynamic_allowed` | `minBudgetTokens`, `maxBudgetTokens`, `canDisable`, `dynamicBudget`; `mode` = `effort` si hay niveles, `budget` si solo hay límites, `unknown` si no |
| presencia de `thinking` | `reasoning = true`; sin `thinking` queda `null` (ausente no es `false`) |
| `supported_parameters` con `tools` | `supportsTools = true`; si no se lista, `null` |
| plantilla `codex_client_models.json` (proyectada a hechos, sin prompts) | solo anota entradas de `codex-*` con el mismo `slug`: `thinking.defaultLevel` y `raw.codexClient`. Un `slug` sin entrada en ningún canal (`gpt-reserve`) no crea observación; `ultra` queda solo en `raw.codexClient` |
| `native_capabilities.web_search` + `native-capabilities-evidence.json` | `raw.nativeCapabilities.webSearch` (tres estados: ausente se queda ausente) y `evidence` del par canal/modelo exacto |
| precio, `authType` | no los publica: `cost = null`, `authType = null` |
| `config` (cabeceras de cliente) | no se lee: es instalación del canal, no catálogo |
| retiradas | el origen no marca ninguna; el script recorre el historial git de `models.json` y `devin_models.json` y emite `lifecycle.status = retired` para lo que existió y ya no está (`raw.retirement`: último commit en que se sirvió, commit y fecha de la retirada). `shutdownOn` queda `null`: la fecha de un commit no es una fecha de cierre. Las entradas presentes son `live` |

Los rechazos del fixture (54) vienen todos de registros retirados (más viejos): `priority` 49, `auto` 4, `ultra` 1.

### `dsh` (registro agregado de dsh-our-free-model)

```sh
node tools/snapshot-git.mjs dsh --repo ~/repositories/dsh-our-free-model --out /tmp/dsh.json   # lee origin/main, no el árbol de trabajo
cargo run --release -p tierbridge-cli -- catalog build --source dsh --input /tmp/dsh.json --out-dir /tmp/tb-out
```

**Desviación respecto al encargo original:** `catalog/` y `feed/` de dsh no contienen modelos (digests de integridad, manifiesto de release y anuncios en HTML). El único dato de modelos es `src/providers/registry.json` (pestaña Aggregate, claves propias), y es lo único que se lee: nunca `vendor/channel-pack` ni código de cuentas, pools o canales. El script fija `origin/main` (en el clon local el árbol de trabajo es v2.2.0 y está desactualizado). No se copian URLs de endpoint, nombres de variables de credenciales, notas ni sitios web.

Es una fuente secundaria (derivada de models.dev, 9router y documentación oficial): confianza `inferred`. La etiqueta del proveedor en origen (`models.dev`, `official`, `9router`, `custom`; en versiones anteriores también `fcc`, free-claude-code, AGPL-3.0 según `docs/sources.md`) se conserva en `raw.registrySource` para que la fusión pueda descartar o rebajar filas por origen.

Fixture: commit `b1142bd` (2026-11-26), 77 proveedores (4 sin modelos) y 511 modelos.

| Campo del origen | Campo de la observación |
|---|---|
| `id` del proveedor / del modelo, `name` | `provider`, `id`, `name` |
| `contextWindow` (53 de 511; el resto `null`) | `contextWindow`; `0`, negativo o no entero es un rechazo |
| `reasoning` (`true`/`false`/`null`) | `reasoning`; `thinking.mode` = `unknown` si razona (el registro dice si razona, no cómo) y `none` si no; `null` queda `null` |
| `vision` (`true`/`false`/`null`) | `input` = `[image, text]` / `[text]` / `null` |
| `auth.kind` del proveedor | `authType`: `apiKey`→`api_key`, `oauth`→`oauth`, `none`→`none`; `adc` (Google ADC, 9 modelos de `vertex` y `vertex-partner`) queda fuera del enum y es un rechazo por modelo; el valor original queda en `raw.authKind` |
| `source`, `sourceUrl`, `category`, `format`, `local` del proveedor | `raw.registrySource`, `raw.registrySourceUrl`, `raw.category`, `raw.format`, `raw.local` |
| precio, límites de salida, ciclo de vida | no los publica: `null` |

### `freellmapi` (registro abierto de tashfeenahmed/freellmapi)

```sh
node --no-warnings tools/snapshot-freellmapi.mjs --repo ~/repositories/freellmapi --out /tmp/fl.json   # Node >= 24
cargo run --release -p tierbridge-cli -- catalog build --source freellmapi --input /tmp/fl.json --out-dir /tmp/tb-out
```

El registro no es un archivo de datos: es el resultado de una cadena de migraciones (semilla, renombres, `UPDATE` y `DELETE` en 25 pasos de datos), así que leer los literales del TypeScript daría un resultado distinto del que sirve el proyecto. El script **repite las migraciones** en una SQLite en memoria y vuelca `models`, `embedding_models`, `quirks` y `quirk_targets`. El adaptador de Rust solo lee ese JSON, nunca TypeScript.

- Se lee solo, con `git show` en el commit fijado: las migraciones, `db/migrate/defaults.ts` (el orden) y `db/model-pricing.ts`. Sin red.
- El TypeScript se desnuda de tipos (`node:module` `stripTypeScriptTypes`) y se evalúa en un `node:vm` que solo ve la base en memoria y unos stubs: `crypto` (funciones puras), `Buffer`, un `process` congelado con `NODE_ENV=production`, `console` mudo y `initEncryptionKey` vacío (escribiría un archivo de clave). `applyModelPricing` es el del proyecto. Se puede reforzar con `node --permission --allow-fs-read=<repo>`.
- Se repiten las **55** migraciones registradas en `DEFAULT_MIGRATIONS`, en ese orden, como hace el runner síncrono del proyecto. El directorio tiene un archivo 56, `20261028_000001_custom_media_endpoint_identity.ts`, que no está registrado ahí y solo toca endpoints de medios personalizados (`media_models` está vacía en la semilla). La cifra «56 migraciones» de `docs/freellmapi-into-cpa.md` cuenta archivos, no migraciones registradas.
- Se descartan las columnas con marcas de tiempo del replay para que el snapshot sea reproducible byte a byte.
- El feed Premium firmado no se consume.

Fixture: commit `0d64e07` (2026-11-25): **108 modelos de chat en 15 plataformas** (openrouter 23, nvidia 13, ollama 11, cloudflare 10, google 9, groq 9, mistral 7, opencode 6, cerebras 4, cohere 4, kilo 4, zhipu 3, huggingface 3, pollinations 1, llm7 1) más 10 embeddings y 11 quirks: **idéntico** a lo medido en `docs/freellmapi-into-cpa.md`. 118 observaciones, 0 rechazos.

| Campo del origen | Campo de la observación |
|---|---|
| `platform`, `model_id`, `display_name` | `provider`, `id`, `name` |
| tabla `models` / `embedding_models` | `kind` = `chat` / `embedding` |
| `context_window` | `contextWindow` (embeddings: `max_input_tokens` → `maxInputTokens`); 0 o negativo es un rechazo |
| `supports_vision`, `supports_tools` (0/1, por defecto 0) | solo un 1 es una afirmación: `input = [image, text]`, `supportsTools = true`. Un 0 no se distingue de «sin clasificar», así que queda `null` (no se convierte en «sin visión») |
| `cost` | `null`: son cuotas de capa gratuita, no precios. Los precios de `model-pricing.ts` son el equivalente de pago más cercano (estimación para el ahorro) y van **solo a `raw.paidEquivalentUsdPerMtok`**; 8 modelos no tienen |
| `rpm/rpd/tpm/tpd_limit`, `monthly_token_budget` | `raw.freeTier` (el esquema no tiene campo de límites de capa gratuita); los `null` no aparecen |
| `intelligence_rank`, `speed_rank`, `size_label` | `raw.ranks` |
| `enabled`, `source` | `raw.enabledByDefault` (11 modelos vienen desactivados), `raw.registrySource` |
| quirks (`quirk_targets`: plataforma + glob con `*`) | `raw.quirks` (slugs) |
| presencia en el registro | `lifecycle.status = live` |

Una columna que falte en una tabla volcada (renombre en origen) es un error estructural, no una tabla de `null`.

### `omniroute`: pendiente (aplazado)

Clon superficial y disperso en `~/repositories/OmniRoute` (`/open-sse/config/providers/` y `providerRegistry.ts`, commit `9671192`, rama `release/v3.8.55`); aún sin adaptador. Queda:

- Un **evaluador de constantes sobre el AST de TypeScript** (no ejecutar el proyecto): el registro usa `spread`, `buildModels([...])`, constantes de `shared.ts` e importaciones de archivos fuera del clon disperso, y lo que no se pueda resolver debe quedar como hueco registrado. `typescript` iría en un `tools/node_modules` ignorado por git, con `package.json` y lockfile versionados (sin instalaciones globales).
- `RegistryModel.unsupportedParams` (y `aliases`, `strip`, `targetFormat`…) no tiene campo en el esquema: van a `raw`. `supportedThinkingEfforts` → `thinking.efforts` (un valor fuera del enum es un rechazo por fila).
- `RegistryModel` no tiene precios: `cost = null`. `contextLength`/`maxInputTokens`/`maxOutputTokens`, `supportsVision/Audio/Video` y `toolCalling` sí se mapean.
- El envelope `tierbridge-snapshot/1` con commit y fecha, y el mismo molde de pruebas (fixture zstd, forma errónea, rechazos).

## Rebanada 3c: catálogo fusionado (`--merge`)

```sh
# todas las fuentes (un --input por --source) y fusión en catalog.json
tierbridge catalog build \
  --source models.dev --input api.json --source modelparams.dev --input modelparams.json \
  --source cliproxyapi-cache --input ~/.omp/agent/cliproxyapi-models.json \
  --source omp-live:easycliproxy --input easycliproxy.json ... \
  --merge [--overrides overrides.json] --out-dir out/
```

- **Papeles.** Las pasarelas (`cliproxyapi-cache`, `omp-live:*`, `router-for-me.models`, `dsh`, `freellmapi`)
  definen qué modelos existen: un `Model` por fila, con clave `<sourceId>:<provider>/<id>`. Las referencias
  (models.dev, modelparams.dev) solo enriquecen. `omp-bundled` es solo de control y nunca gana. Las
  retiradas de `router-for-me.models` significan "quitado de ese canal": no crean filas ni degradan otras.
- **Precedencia por campo** según `docs/catalog-proof.md` §9: `configured` (overrides) > pasarela sin
  valor de relleno > primera parte de la referencia > mayoría estricta de revendedores > null. Los
  valores de relleno de cada pasarela (p. ej. 128000/32768 en easycliproxy) se detectan y solo van a
  `alternates`. Cada campo guarda regla, confianza, `conflict` y alternativas.
- **Coste.** Un 0 de pasarela nunca significa gratis. Sin evidencia, `{status: "unknown"}`.
- **Salidas.** `catalog.json` (Model[], válido contra el esquema) y `report.merge.json` (filas y
  emparejamiento por pasarela, histograma de reglas ganadoras, conflictos por campo, estudio
  `range.max` vs `limit.output`, diagnósticos `thinking-toggle` y `unmodelled`).
- **Con los snapshots del 2026-11-26:** 4 355 modelos, 75,5 % emparejados, 0 valores de relleno
  ganadores, coste conocido/gratis/desconocido 2 176/342/1 837, 28 MB.

## Rebanada 5b: `tierbridge export cpa`

Genera el bloque `openai-compatibility` de CLIProxyAPI (`config-version: 8`) a partir de un `catalog.json`
fusionado, para una pasarela:

```sh
export EASYCLIPROXY_API_KEY=...        # clave del upstream; solo se lee de una variable de entorno
tierbridge export cpa \
  --catalog out/catalog.json --gateway omp-live:easycliproxy \
  --base-url http://127.0.0.1:8317/v1 \
  --api-key-env EASYCLIPROXY_API_KEY --out cpa-tierbridge.yaml     # 0600
```

- **Qué emite.** Un proveedor (`name:` por defecto `tierbridge-<pasarela>`) con un `models[]` por fila de
  la pasarela: `name` = id que sirve la pasarela, `alias` = `--alias-prefix` (por defecto `tb-`) + id,
  y lo que la fusión resolvió: `max-context-length`, `input-modalities`/`output-modalities`,
  `display-name` (si difiere del id) y `thinking.levels` (solo modelos con esfuerzos discretos; `off`
  no es un nivel). Los campos desconocidos se omiten: nunca se inventa un límite.
- **Qué omite** (resumen por stderr): `kind` conocido distinto de `chat`, `lifecycle` `retired` y alias
  repetidos (gana la primera fila por clave). `deprecated` se exporta: la pasarela lo sigue sirviendo.
- **Es un fragmento**, no una config: cuelga de la raíz `api-keys:` (`openai-compatibility` bajo
  `api-keys`; la forma plana de v7 no se genera). CLIProxyAPI no tiene `include`, así que se pega en
  `config.yaml` o se concatena a una config base, como hace la prueba de aceptación.
- **Clave.** `--api-key-env VAR` copia el valor de `VAR` al fichero, por eso exige `--out` (nunca se
  imprime en stdout) y el fichero se crea con modo 0600 (uno existente se restringe). Sin ella se escribe
  el marcador `REPLACE_WITH_UPSTREAM_API_KEY` y un aviso. En `omp`, `apiKey: <NOMBRE>` de `models.yml`
  es el nombre de una variable de entorno, no la clave.
- **Salida determinista**: ordenada por alias, sin marcas de tiempo.

**Aceptación (2026-11-27, CLIProxyAPI 8.1.7).** Catálogo fusionado de `omp models easycliproxy` en vivo
(1 699 modelos) con models.dev y modelparams.dev; exportadas 1 663 filas (36 `retired` omitidas). Una
instancia de CLIProxyAPI aparte en `127.0.0.1:18417` con esa salida y un `access.api-keys` propio:
`GET /v1/models` devolvió los 1 663 alias (`owned_by: tierbridge-omp-live-easycliproxy`) y un
`POST /v1/chat/completions` a `tb-openai/gpt-oss-20b` llegó por el easycliproxy vivo
(`127.0.0.1:8317`) y devolvió `ok` (200, 4,5 s). Pasar por la instancia aparte también expone el estado
real del upstream: `tb-agnes-2.0-flash` da 500 (`get_channel_failed`) y modelos bloqueados por cuenta
dan 429; el alias se resuelve igual, y el fallo es del canal, no de la exportación.

**Límites conocidos.** Los alias repetidos forman en CLIProxyAPI un pool round-robin, no ordenado por
ranking (`docs/freellmapi-into-cpa.md`); este comando no agrupa filas en pools. No cubre los overrides de
`models.yml` de omp. No se ha comprobado `omp models` contra la instancia aparte (sus alias llevan el
prefijo `tb-`).

## Rebanada 5a: overrides de omp (`export omp`, `apply omp`, `snapshot omp-live`)

omp rellena con valores de relleno (128000/32768, precio 0 con estado `unknown`) los modelos que descubre en
`easycliproxy` y `dsh` (`discovery: {type: openai-models-list}`). Esto lleva lo que el catálogo fusionado sí
sabe a `providers.<p>.modelOverrides` de `models.yml`:

```sh
# 1. lo que omp muestra hoy, sin que nuestros overrides se lean de vuelta (copia de models.yml sin bloques)
tierbridge snapshot omp-live --provider easycliproxy --out easy.json
tierbridge snapshot omp-live --provider dsh --out dsh.json
# 2. catálogo fusionado (ver «Rebanada 3c») y overrides
tierbridge catalog build --source models.dev --input api.json --source modelparams.dev --input modelparams.json \
  --source omp-live:easycliproxy --input easy.json --source omp-live:dsh --input dsh.json --merge --out-dir out/
tierbridge export omp --catalog out/catalog.json --provider easycliproxy --provider dsh \
  --current easy.json --current dsh.json [--with-thinking] --out overrides.yml
# 3. editar models.yml (con --dry-run no escribe nada; --revert quita los bloques)
tierbridge apply omp --models-yml ~/.omp/agent/models.yml --overrides overrides.yml [--dry-run]
tierbridge apply omp --models-yml ~/.omp/agent/models.yml --revert
```

- **`export omp`.** Por modelo de `omp-live:<p>` emite `contextWindow`, `maxTokens`, `cost` (conocido o
  gratis; el desconocido se omite) e `input` (solo `text`/`image`). Un campo sale solo si su ganador es
  evidencia real (no `none`, no relleno) **y** no es el valor de la propia pasarela (omp ya lo muestra) **y**,
  con `--current`, difiere de lo que omp muestra. `reasoning` y `thinking` solo con `--with-thinking`
  (`thinking` solo en modo `effort`, sin `off`, que omp no tiene). No exporta los tramos de precio (`tiers`).
  La salida es determinista (sin reloj) y lleva en la cabecera el sha256 del catálogo y los conteos por
  campo; por stdout, cuántos modelos conservan el relleno y por qué (sin coincidencia, referencias en
  desacuerdo, coincidencia sin límite).
- **`apply omp`.** Edición **de texto**: las demás líneas y comentarios quedan byte a byte. Inserta, dentro de
  cada proveedor, un bloque entre `# >>> tierbridge-managed modelOverrides (do not edit) >>>` y
  `# <<< tierbridge-managed modelOverrides <<<`, con la sangría del propio archivo. Fija el estado
  gestionado a lo que dice el archivo (quita todos los bloques anteriores), así que aplicarlo dos veces da
  los mismos bytes y no escribe nada. Rechaza un proveedor con `modelOverrides` propio, un proveedor ausente,
  proveedores en línea (`{...}`) y marcadores desparejados. Antes de escribir: copia
  `models.yml.tierbridge-<UTC>.bak` con el mismo modo (el archivo lleva claves), escritura atómica, y el
  texto editado se parsea como YAML y debe diferir del original solo por los `modelOverrides` gestionados.
  `--revert` devuelve los bytes originales.
- **`snapshot omp-live`.** Copia **solo** `models.yml` a un directorio temporal 0700, le quita los bloques
  gestionados, ejecuta el omp 19.2.1 (`$TIERBRIDGE_OMP` o la instalación de Tern; nunca el `omp` del PATH)
  con `PI_CODING_AGENT_DIR` apuntando ahí, valida el roster y borra el directorio. El stderr de omp no se
  reenvía tal cual (las palabras con aspecto de credencial se ocultan).

**Aceptación (2026-11-27, sobre una copia del directorio del agente con `models.yml`, `models.json` y
`models.db`; `~/.omp/agent/models.yml` intacto, comprobado por sha256).** `PI_CODING_AGENT_DIR=<copia> omp
models <p> --json`. Para easycliproxy hizo falta la caché `models.db`: el CLIProxyAPI vivo respondía 401 a la
clave de `models.yml`, así que su roster salió de esa caché (dsh, 200, salió en vivo):

| | antes | después |
|---|---|---|
| easycliproxy: modelos / `ctx=128000` / `maxTokens=32768` / precio `unknown` | 1699 / 712 / 734 / 1699 | 1699 / 489 / 541 / 914 |
| dsh: modelos / `ctx=128000` / `maxTokens=32768` / precio `unknown` | 51 / 20 / 21 / 51 | 51 / 5 / 7 / 44 |

Se escribieron 980 modelos (easycliproxy: 223 `contextWindow`, 193 `maxTokens`, 910 `cost`) y 45 (dsh: 15, 14,
45), y cada límite exportado aparece tal cual en omp (416 de 416 en easycliproxy, 29 de 29 en dsh); ningún otro campo cambió y omp
no emite avisos nuevos. Los `unknown` que quedan son 789 y 6 sin evidencia de precio más los de precio
gratis (125 y 38): omp muestra `unknown` para un precio exportado todo a cero, y `fixed` solo para los 785 y
7 con precio. Aplicar dos veces dio los mismos bytes (sin segunda copia de respaldo), `--revert` devolvió el
sha256 original y un `snapshot omp-live` de la copia ya editada reproduce la línea base (dsh: 51/20/21/51).

**Límites conocidos.** Quedan con relleno 471 modelos de easycliproxy y 5 de dsh (376 y 3 sin coincidencia en
las referencias). `snapshot omp-live` copia solo `models.yml`: si la pasarela rechaza la clave de ese
archivo (hoy CLIProxyAPI responde 401 a la de `easycliproxy`), omp no descubre nada y el comando falla con
«no non-empty `models` array» en lugar de usar un roster viejo. Los tramos de precio (142 modelos de
easycliproxy) no se exportan; `thinking` de modo `toggle`/`budget` no tiene forma en `ThinkingConfig` de omp.

## Rebanada 4: pipeline de sincronizado (`tierbridge sync`)

Mantiene al día, sin intervención y sin aplicar nada solo, lo que las demás rebanadas calculan (`docs/design.md` §3 y §6.4).
Los datos viven en un **repo git privado y aparte** (las listas de las pasarelas del autor no se publican): cada sincronizado abre
una rama `sync/<fecha>` y un PR con el diff clasificado; fusionar el PR es aceptar. Aplicar a omp es un comando manual.

```sh
tierbridge sync init --data-repo ~/repositories/llm-tierbridge-data [--remote git@github.com:blyzer/llm-tierbridge-data.git]
tierbridge sync run   --data-repo ~/repositories/llm-tierbridge-data [--source S]... [--force] [--no-push] [--no-pr]
tierbridge sync apply --data-repo ~/repositories/llm-tierbridge-data --models-yml ~/.omp/agent/models.yml [--dry-run]
tierbridge sync install-launchd [--hour 6] --data-repo ~/repositories/llm-tierbridge-data [--dry-run]
tierbridge sync uninstall-launchd [--dry-run]
```

**Repo de datos** (lo crea `sync init`, con un commit en `main`):

| Ruta | Contenido |
|---|---|
| `sync-state.json` | por fuente: `revision` (etag, commit git o `sha256:`), `sha256`, `fetchedAt`, `count`, `cadence`, `lastSyncedAt`. Solo avanza dentro del PR |
| `index/<fuente>.jsonl` | una línea ordenada por observación (`provider`, `id`, `authType`, límites, coste, ciclo de vida, esfuerzos, modalidades de entrada, `kind`); sin marcas de tiempo, así que `git diff` solo muestra cambios reales. `omp-live:dsh` se guarda como `omp-live_dsh.jsonl` |
| `overrides/omp.yml` | salida de `export omp` con `--current` de los rosters `omp-live` |
| `reports/<fecha>.md` y `.json` | diff clasificado y resumen de la ejecución (el `.md` es el cuerpo del PR) |
| `overrides.json` | excepciones escritas a mano (`configured`, ver «Rebanada 3c»); `{}` al crear |

Las descargas en bruto, los clones git y el `catalog.json` fusionado **no se versionan**: están en `~/.cache/llm-tierbridge/`
(`raw/<sha256>`, `repos/`, `last/`, `latest.json`; se cambia con `--cache-dir` o `$TIERBRIDGE_CACHE_DIR`).

**`sync run`**, por orden: (a) refresca solo las fuentes que tocan; (b) valida; (c) clasifica el diff contra `index/`; (d) fusiona
todas las fuentes con `overrides.json`, regenera `overrides/omp.yml` y, si algo cambió, crea la rama, hace commit, empuja y abre el
PR con `gh pr create` (`--no-pr`: solo empuja; `--no-push`: solo commit local). Sin cambios no crea rama: «nothing to sync».
Si ya hay una rama `sync/*` sin fusionar con exactamente lo mismo, tampoco crea otra. Nada se escribe en el repo de datos hasta
que todo lo anterior salió bien.

| Fuente | Cadencia | Cómo se obtiene |
|---|---|---|
| `models.dev` | diaria (≥ 20 h) | `https://models.dev/api.json` |
| `modelparams.dev` | semanal (≥ 6 días) | `https://modelparams.dev/api/v1/models.json` |
| `cliproxyapi-cache` | diaria | `~/.omp/agent/cliproxyapi-models.json` (solo lectura) |
| `omp-live:easycliproxy`, `omp-live:dsh` | diaria | `tierbridge snapshot omp-live` (necesita la clave en el entorno) |
| `router-for-me.models`, `dsh`, `freellmapi` | cuando cambia el `HEAD` del upstream (`git ls-remote`) | clon en `~/.cache/llm-tierbridge/repos/` + `tools/snapshot-git.mjs` / `tools/snapshot-freellmapi.mjs` (Node ≥ 24). `dsh` y `freellmapi` con `--depth 1`; `router-for-me.models` con historial completo, porque su lista de retirados sale de él |

`omp-bundled` no se sincroniza (es solo de control: nunca gana un campo). La última instantánea de cada fuente se recuerda en
`latest.json`, así que una fuente que no toca se reutiliza de la caché y no se vuelve a descargar. `--input FUENTE=ARCHIVO`
usa una instantánea guardada en lugar de descargar (así corren las pruebas, sin red).

**Falla en alto.** Un cambio de formato aguas arriba (el adaptador no entiende el documento), una fuente sin modelos o una que
**encoge más del 20 %** respecto a `sync-state.json` (`--max-shrink-percent N`) hacen que `sync run` salga con código ≠ 0,
escriba una línea con fecha en stderr, lance una notificación de macOS (`osascript`; se omite con `--no-notify` o
`$TIERBRIDGE_NO_NOTIFY`) y **no toque el repo de datos** (sin rama, `git status` limpio). La prueba
`a_format_change_fails_loud_and_leaves_the_data_repo_untouched` lo comprueba.

**Diff clasificado**, por fuente: añadidos, retirados, límite, coste, ciclo de vida, esfuerzos y otros cambios; los modelos que no
son de chat (`kind` distinto de `chat`) van aparte como «fuera de alcance». El informe lista hasta 20 por clase y el `.json` hasta 200.

**`sync apply`** hace `git pull --ff-only origin main` en el repo de datos (debe estar en `main`; con `--dry-run` también tira, solo
protege `models.yml`) y luego el `apply omp` de la rebanada 5a con `overrides/omp.yml`: copia de seguridad, idempotente.

**launchd.** `install-launchd` escribe `~/Library/LaunchAgents/dev.blyzer.llm-tierbridge.sync.plist` (diario a las 6:00; registros en
`~/Library/Logs/llm-tierbridge/sync.{out,err}.log`) y un script envoltorio en
`~/Library/Application Support/llm-tierbridge/sync-launchd.sh`, y lo carga con `launchctl bootstrap` (`--no-load` no lo carga;
`--dry-run` solo imprime ambos). launchd no hereda el entorno, y el descubrimiento de omp necesita claves (p. ej.
`EASYCLIPROXY_API_KEY`): el envoltorio las lee del llavero de macOS antes de lanzar `sync run`. Para añadir una:

```sh
security add-generic-password -s llm-tierbridge -a EASYCLIPROXY_API_KEY -w     # pide el valor; no lo escribas en la línea de comandos
tierbridge sync install-launchd --data-repo … --keychain-var EASYCLIPROXY_API_KEY --keychain-var OTRA_CLAVE
```

La lista de variables (`--keychain-var`, repetible; por defecto solo `EASYCLIPROXY_API_KEY`) queda escrita en el envoltorio: para
cambiarla se vuelve a ejecutar `install-launchd`. Si falta un elemento del llavero, el envoltorio avisa por stderr y sigue: el
descubrimiento de omp falla y la ejecución falla en alto. `uninstall-launchd` descarga y borra el plist y el envoltorio.

Requisitos del trabajo programado: `git`, `gh` (con sesión), `node` ≥ 24 y la instalación de omp 19.2.1 de Tern; el envoltorio fija
`PATH=/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin`.

## Pruebas

```sh
cargo test --workspace                        # vista pública: todo lo que corre solo con el árbol publicado
cargo test --workspace -- --include-ignored   # vista completa: además, las pruebas que necesitan datos privados
```

Las pruebas marcadas `#[ignore = "private fixtures…"]` leen datos que **no se publican**: los rosters de las
pasarelas del autor (`cliproxyapi-models`, `omp-models-<proveedor>` y las filas de referencia `obs.*` del
prototipo) y la instantánea de `router-for-me/models` (sin licencia). Se buscan, con los mismos nombres que
tenían en `crates/catalog/tests/fixtures/`, en `local-fixtures/` (en la raíz, ignorado por git) o en el
directorio de la variable de entorno `TIERBRIDGE_LOCAL_FIXTURES`; las cifras del prototipo salen de
`prototype/out/results.json` (regenerable con `prototype/compare.py`, también ignorado). Sin esos archivos,
`--include-ignored` falla con un mensaje que nombra el que falta; la vista pública no los necesita: esos adaptadores (`cliproxyapi-cache`, `omp-live`, `router-for-me.models`) se prueban ahí con
muestras sintéticas de ≤40 filas y ids públicos en `crates/catalog/tests/fixtures/samples/` (`crates/catalog/tests/samples.rs`).

## Documentos

| Archivo | Contenido |
|---|---|
| `docs/design.md` | Diseño y rebanadas |
| `docs/freellmapi-vs-cpa.md` | Cómo funciona cada uno y qué toma este proyecto de cada uno |
| `docs/freellmapi-into-cpa.md` | Mapeo de funciones, archivos calientes, front, ingesta del catálogo y opciones de fork |
| `docs/cpa-plugin-smoke.md` | Prueba de carga de plugins |
| `docs/cpa-providers.md` | Comparativa de los 12 proveedores omp/pi para CLIProxyAPI |
| `docs/sources.md` | Fuentes, licencias y riesgos |
| `docs/catalog-proof.md` | Prueba del catálogo normalizado |
| `docs/usage-and-language.md` | Uso de tokens y elección de lenguaje |

## Licencia

MIT, ver `LICENSE` (copyright 2026 blyzer). Las fuentes de datos de terceros de los fixtures de prueba y sus licencias están en `NOTICE`.

## Rebanada 6: `tierbridge export cpa-router` y el plugin `cpa-tierbridge`

Un alias de modelo, `tierbridge/auto`, que CLIProxyAPI resuelve por ranking: el primer candidato gratuito (o el más
barato) que cabe en la petición. Son dos piezas: un comando que calcula el ranking y un plugin nativo de
CLIProxyAPI que lo aplica (`plugins/cpa-tierbridge/`, en Go; su README tiene el detalle de configuración).

### `tierbridge export cpa-router`

```sh
tierbridge export cpa-router \
  --catalog out/catalog.json --gateway omp-live:easycliproxy \
  --name tierbridge-omp-live-easycliproxy --alias-prefix tb- \
  --alias tierbridge/auto --profile free-first \
  --freellmapi-observations out/observations.freellmapi.jsonl \
  --out tierbridge-router.json     # 0600
```

Escribe `tierbridge-router.json` (esquema `tierbridge-router/1`): la lista ordenada de candidatos de un alias.

- **Mismos `--name` y `--alias-prefix` que `export cpa`.** Los candidatos son las filas de **una** pasarela, la que
  `export cpa` vuelca en CLIProxyAPI: `candidate.model` es el alias que `export cpa` registra (`--alias-prefix`, por
  defecto `tb-`, más el id) y `candidate.provider` es la clave de proveedor que CLIProxyAPI deriva de `--name`
  (`openai-compatible-<name en minúsculas>`; por defecto `tierbridge-<pasarela>`). Si no coinciden, el plugin
  enruta a un modelo que el proveedor no conoce.
- **`--freellmapi-observations`, y por qué.** El `catalog.json` fusionado no lleva `raw`, así que los límites del
  nivel gratuito (`rpm`, `rpd`, `tpm`, `tpd`) no están en él: salen de `observations.freellmapi.jsonl`
  (`catalog build --source freellmapi`). Sin esa opción todo `quota` es `null` (ilimitado) y se avisa por stderr;
  con ella se informa cuántos candidatos cruzaron con una observación.
- **Perfil `free-first`** (el único hoy): gratuitos conocidos primero; luego coste conocido ascendente
  (`input + output`, USD por millón de tokens); luego coste desconocido. Los empates se rompen por confianza de la
  evidencia, ventana de contexto descendente (`null` al final), `id` y clave del catálogo. Un coste «conocido» pero
  incompleto cuenta como desconocido: el plugin nunca multiplica por un precio ausente.
- **Exclusiones** (resumen por stderr y en `skipped`): `kind` conocido distinto de `chat` (`not-chat`), `lifecycle`
  `retired` (`retired`), filas cuyos límites son solo marcadores de posición de la pasarela
  (`placeholder-limits`) y alias repetidos (`duplicate-alias`; gana la primera fila por clave, como en `export cpa`).
- Salida determinista y sin marcas de tiempo; `--out` crea el fichero con modo 0600.

### El plugin

- **`ModelRouter` solo para el alias.** Atiende `tierbridge/auto` (clave `alias`) y nada más: cualquier otro modelo
  vuelve con `Handled:false` y la cadena sigue como si el plugin no existiera. Si ningún candidato cabe, o hay
  cualquier error del plugin (también un `panic`, que se captura), responde `Handled:false`: el host trata entonces
  el alias como un modelo desconocido y contesta `400 model_not_found`. Un candidato cabe si su ventana de
  contexto admite el prompt estimado (bytes de texto / 4), soporta herramientas e imágenes si la petición las usa,
  su proveedor tiene credenciales en el host, no está en enfriamiento, el gasto estimado cabe en el presupuesto
  diario (los gratuitos nunca se saltan por presupuesto) y le quedan `rpm`/`rpd`/`tpm`/`tpd`.
- **`UsagePlugin`: contabilidad.** Con los registros de uso del host cuenta tokens (`tpm` en ventana deslizante de
  60 s, `tpd` por día UTC), gasto diario (`daily-budget-usd`; precio desconocido cuenta 0 y se lista como
  `unpriced`) y pone en enfriamiento (`cooldown-seconds`, o un `Retry-After` mayor) al modelo cuyo registro sea
  `Failed` con 429 o >= 500. El estado se guarda de forma atómica en `state-path` (JSON, modo 0600) en el plazo de ~1 s tras un
  cambio y, de forma síncrona, en `plugin.quiesce`, `plugin.shutdown` y el gancho C de apagado; que el host invoque
  ese gancho al recibir SIGTERM no está verificado hasta repetir la prueba de aceptación.
- **Rutas de gestión autenticadas**, solo lectura: `/tierbridge/status`, `/candidates`, `/counters`, `/budget` y
  `/decisions?limit=N` (bajo el prefijo de gestión del host, p. ej. `/v0/management/tierbridge/status`). No hay
  `QuotaProvider`: la API de cuotas del host es **por credencial** (`auth_index`), y aquí el límite, el
  enfriamiento y el presupuesto son **por modelo**; por eso se exponen como JSON de gestión.

### Despliegue recomendado

- **Fijar CLIProxyAPI 8.1.7** (`47f18e4d`, ABI nativa 1, esquema RPC 10): la ABI y el esquema cambian a menudo; hay
  que reconstruir y reverificar el plugin con cada versión del host.
- **`plugins.configs.tierbridge.priority` explícito.** Los 32 plugins del host vivo están en prioridad 0 y sin ella
  en su configuración; el orden de la cadena es prioridad descendente y luego id ascendente, es decir, alfabético.
  Nuestro router solo contesta al alias, así que no toca el resto del tráfico; fijarla evita depender del orden
  alfabético frente a otros `ModelRouter`.
- **Un `panic` inutiliza el plugin** (el host lo «funde» hasta reiniciar): por eso el plugin captura los `panic`
  en cada entrada (ruteo, uso, gestión y la frontera cgo).

### Lo observado en una instancia aparte (8.1.7, reales)

Prueba de aceptación en una instancia desechable en `127.0.0.1:18417`
(`plugins/cpa-tierbridge/scripts/scratch-acceptance.sh`): 3 candidatos gratuitos reales sobre el upstream de
`127.0.0.1:8317`, 8 de 10 peticiones de chat usadas:

- El host **pregunta al router una vez por petición** y ejecuta contra el `{proveedor, modelo}` fijado. **No vuelve a
  preguntar ni cae a otro candidato** si el modelo falla: un id inexistente da `503 auth_not_found`, y un `429` del
  upstream llega tal cual al cliente. Un candidato que falla solo se evita en la **petición siguiente**, vía
  enfriamiento (registro de uso `Failed` 429/>= 500, `cooldown-seconds`, `Retry-After`).
- El registro de uso de una petición enrutada lleva el **id del upstream sin el prefijo `tb-`** (no el alias ni
  `tb-…`). El plugin asocia por `Model`, luego `Alias`, y luego id del upstream más proveedor, y conserva los
  últimos registros vistos en `lastUsageModels` del estado.
- Con `debug-force-error` el `panic` se traga y el alias da `400 model_not_found` con el host vivo; el tráfico que
  no es del alias no se altera. La petición al alias sigue al candidato superior que cabe, y una segunda petición
  salta al candidato con `quota.rpm: 1` ya gastado.
- El host **no registra una línea de capacidades** y `/v0/management/plugins` no muestra ninguna (solo
  `supports_oauth` y `supports_quota`).

### Lo que NO está verificado

- Un despliegue en la instancia viva, la recarga en caliente, el comportamiento ante fallos en *streaming*, y otras
  plataformas distintas de macOS/arm64.
- Las rutas de gestión, `lastUsageModels` y el enfriamiento extremo a extremo en el host: esa corrida se hizo con el
  plugin de la primera entrega (sin contabilidad ni gestión; las cinco rutas daban 404 con el secreto y 401 sin él).
  Queda repetirla con la versión actual (`REQUIRE_MGMT=1`); las rutas están cubiertas por pruebas unitarias.
- Las cadenas de los registros de uso más allá de lo que mostró esa corrida (modelo del upstream sin `tb-`).

**Fase 2 (opcional, no implementada):** un `Scheduler` que solo filtre credenciales.
