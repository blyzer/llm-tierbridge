# Paquetes "provider" de CLIProxyAPI: comparación (2026-11-26)

Estado: **investigación, solo lectura**. Objetivo: decidir qué funciones entran en el paquete propio (CORE = fork Go de
CLIProxyAPI "CPA"; CATALOG = normalizador models.dev + modelparams.dev + router-for-me/models; FRONT = Management Center;
adaptador de host omp/pi/dsh). Contexto previo: `design.md`, `sources.md`, `catalog-proof.md`, `usage-and-language.md` (este directorio).

Método: se clonaron (`--depth 30`) los 5 repos de GitHub y se hizo `npm pack` de los 7 paquetes npm en `/tmp/cpa-prov/{git,npm}`
(ningún clon falló). Se leyó código, no README. Evidencia como `ruta:línea` relativa a la raíz de cada candidato; `CPA:` = `~/repositories/CLIProxyAPI`.
Lo que no pude verificar está marcado **pendiente** (ver §6).

## 0. Línea base: qué expone CPA hoy (verificado en el código de CPA)

Esto fija el techo de cualquier provider: ninguno puede enriquecer con datos que CPA no entrega, salvo que los traiga de fuera.

| Superficie | Qué devuelve | Evidencia |
|---|---|---|
| `GET /v1/models` (sabor OpenAI) | **solo** `id, object, created, owned_by`: el handler descarta `context_length`, `max_completion_tokens`, `supported_parameters` que el registro sí tiene | `CPA:sdk/api/handlers/openai/openai_handlers.go:82-109` (el registro los construye en `CPA:internal/registry/model_registry.go:2059-2070` y el handler los filtra) |
| `GET /v1/models?client_version=X` | catálogo **estilo Codex**: `slug, context_window, max_context_window, max_tokens, supported_reasoning_levels, input_modalities, visibility` | enrutado en `CPA:internal/api/server_routes.go:660-667`; campos en `CPA:internal/client/codex/models/models.go:131,451,546-553,730-731,855-901` |
| `/v1/models` con `Anthropic-Version` o UA `claude-cli` | `max_input_tokens` / `max_tokens`, con **relleno por defecto 200000 / 64000** | `CPA:internal/api/server_routes.go:641-646`; `CPA:internal/registry/model_registry.go:24-25,2088-2097` |
| `GET /v1beta/models` (Gemini) | `inputTokenLimit`, `outputTokenLimit`, modalidades | `CPA:internal/registry/model_registry.go:2116-2138` |
| `ModelInfo` | tiene `Thinking` y modalidades; **no tiene campo de coste** (L40-116) | `CPA:internal/registry/model_registry.go:87-103` |
| Gestión `/v0/management/model-definitions/:channel` | `ModelInfo` completo por canal (incluye `thinking`) | `CPA:internal/api/server_management.go:182`; `.../handlers/management/model_definitions.go:63-101` |
| Gestión `/v0/management/auth-files/models?name=` | por credencial: solo `id, display_name, type, owned_by` | `CPA:internal/api/server_management.go:181`; `.../management/auth_files.go:336-381` |
| Cuotas por credencial **ya en CPA** | `GET /quota/providers`, `POST /quota/fetch`, `POST /quota/reset`, cuota de plugins, `usage-queue` | `CPA:internal/api/server_management.go:43-46,84-86,93` |
| Estrategia de enrutado | `GET/PUT /routing/strategy` | `CPA:internal/api/server_management.go:131-133` |

Consecuencias: (1) los providers DSH/pi usan el **catálogo Codex** (`?client_version=`) porque es la única ruta pública con límites y niveles
de razonamiento; (2) ninguna ruta pública da **coste** ni **procedencia**; (3) cuota multicuenta y enrutado **ya viven en el CORE**, así que
reimplementarlos en un provider duplica trabajo.

## 1. Tabla comparativa

Licencia "efectiva" = lo que permite copiar código: archivo `LICENSE` con titular. Un `"license": "MIT"` solo en `package.json`, sin texto ni titular, **no** se considera licencia.

| # | Candidato | Origen | Licencia efectiva | Última actividad | Host | Descubre en CPA | Enriquece de | Protocolos |
|---|---|---|---|---|---|---|---|---|
| 1 | router-for-me/dsh-cliproxyapi-provider (oficial) | GitHub, 3 commits, v0.0.2, `private:true` | **ninguna** (sin LICENSE en ningún commit; API: `null`; `package.json` dice MIT) | push 2026-08-22 | DSH | `{base}/models?client_version=dsh-cliproxyapi-provider` | solo CPA; defaults 262144/32768 | solo `openai-responses` |
| 2 | ChadCSong/dsh-cliproxyapi-provider | GitHub, 12 commits, 2★ | **MIT** (LICENSE, "ChadCSong and … contributors") | push 2026-10-19 | DSH 0.2 | `/v1/models?client_version=pi` + autodetección 127.0.0.1/localhost/[::1]:8317 | solo CPA; sin tabla ni defaults | completions (def.) / responses |
| 3 | pd90506/dsh-cliproxyapi-provider | GitHub, 2 commits, 0★ | **MIT** (LICENSE, "pd90506") | push 2026-09-18 | DSH | `/v1/models?client_version=pi`, solo `claude-*` (+ ruta xAI) | CPA ?? tabla de 16 filas ?? 200000/16384 | Anthropic messages (adaptive thinking) |
| 4 | Factory48/dsh-cliproxyapi-provider | GitHub, 3 commits (continuación lineal del oficial) | **ninguna** (MIT solo en `package.json`) | commit 2026-10-04, push 2026-10-18 | DSH 0.2 | igual que el oficial | igual que el oficial | solo `openai-responses` |
| 5 | lingyired/dsh-llm-cliproxyapi | GitHub, 1 commit | **ninguna** | 2026-10-07 | DSH (solo parche de rutas) | no descubre: rutas declaradas a mano | n/a | responses / completions según ruta |
| 6 | npm @gray0128/dsh-cliproxyapi-provider 0.0.6 | npm, 5 versiones | **ninguna** (sin LICENSE en el tarball) | 2026-10-29 | DSH 0.2.1-rc | catálogo CPA | CPA, **y calibra con pi-ai, OpenRouter y LiteLLM** | solo `openai-responses` |
| 7 | npm @thebtf/dsh-cliproxyapi-provider 0.5.6 | npm, 15 versiones | MIT (LICENSE, Kirill Turanskiy) pero **deriva del esqueleto del oficial sin atribución** | 2026-11-23 | DSH (GUI, Node ≥24.9) | `/v1/models?client_version=dsh-cliproxyapi-provider` | CPA + overrides por modelo con procedencia; sin descarga remota | 4: responses, completions, anthropic-messages, google-generative-ai |
| 8 | npm dsh-cliproxyapi-pro 1.12.1 | npm (repo TinyBOOOOM), 15 versiones, 976 KB | MIT (LICENSE, "bowen") | 2026-11-17 | DSH (multicuenta, cuotas, GUI) | `/v1/models` y `/v1/models?client_version=pi` (`lib/host/discovery.js:207,265`) | CPA + models.dev y OpenRouter opcionales, TTL 24 h (`lib/host/model-info.js:14-17`) | completions/responses + Gemini nativo `<ruta>-gemini` (CHANGELOG 1.12.1) |
| 9 | npm dsh-model 0.38.6 | npm (repo AIcivilization), 52 versiones, 1.3 MB | MIT (LICENSE, "AIcivilization") | 2026-11-26 | DSH + demonio propio | no descubre: **embebe CPA 8.0.13 sin modificar** como motor (`engine-manifest.json:1-10`) | `free-catalog.json` (725 líneas, `updatedAt` 2026-11-19) | relé propio `free/auto, free/smart, free/fast` (`dist/free/service.js:4`) |
| 10 | npm pi-cliproxyapi-provider 0.16.24 (0xRichardH) | npm, 111 versiones, 7.4 MB | MIT (LICENSE, "Richard Hao") | 2026-11-25 | pi | `GET /v1/models`, exige `data[]` (`src/cpa.ts:47,59`) | **models.dev `api.json`** (`src/models-dev.ts:5,44`) + copia empaquetada `data/models-dev-fallback.json` (7.0 MB) | los delega a pi-ai (no verificado) |
| 11 | npm @router-for-me/pi-cliproxyapi-provider 1.5.0 | npm oficial, 36 versiones | MIT (LICENSE, "Router-For.ME") | 2026-11-18 | pi (`~/.pi/agent/settings.json`) | `/v1/models?client_version=pi` (`extensions/lib.ts:192,222-223`) | CPA + **coste de models.dev** con tramos y modo rápido (`lib.ts:151-155,881-883,927`); defectos 128000/16384 (`lib.ts:35-36`) | flujo Codex (`codex-stream.ts`); resto pendiente |
| 12 | npm @izhimu/pi-cliproxyapi-provider 1.5.0-1 | npm, 6 versiones; fork del #11 | MIT (LICENSE conserva "Router-For.ME") | 2026-11-19 | pi/omp | igual que #11 (`extensions/lib.ts:191,221-222`) | igual que #11 + caché de coste en memoria | igual que #11 |

Resumen de linaje (comprobado con `git merge-base` y centinelas compartidos `PLACEHOLDER_AUTHORIZATION`, `x-dsh-provider-cpa-sync`, `takeDiscovery`, `readBoundedJson`):
**oficial → {Factory48, @gray0128, @thebtf}**. ChadCSong y pd90506 **no** comparten esqueleto (0 coincidencias de `x-dsh-provider-cpa-sync`). lingyired no comparte código con nadie.

## 2. Notas por candidato (con evidencia)

### 2.1 router-for-me/dsh-cliproxyapi-provider (oficial)
- **Licencia**: sin LICENSE en `git ls-tree`, `git log --all` ni checkout; `package.json` declara MIT y `private:true` (nunca se publicó en npm). Tratar como "todos los derechos reservados" hasta confirmación del autor.
- **Descubrimiento**: `src/catalog.js:81-86` arma `{base}/models?client_version=dsh-cliproxyapi-provider`; `src/catalog.js:67-79` exige `models[]` y falla si está vacío.
- **Enriquecimiento (solo campos de CPA)**: ventana = `max_context_window ?? context_window ?? default` (`src/catalog.js:60`); salida = `max_output_tokens ?? max_completion_tokens ?? max_tokens ?? default` (`:61`); modalidades solo `text|image` (`:39-51`); niveles de razonamiento normalizados, `none→off`, `ultra→max`, y descarta mapas que solo ofrecen `off` (`:17-37`).
- **Defaults cuando CPA no dice nada**: 262144 / 32768 (`src/index.js:23-24`). Estos defaults son justo el "límite inventado" que el catálogo propio debe sustituir por `unknown`.
- **Protocolo**: fijo `openai-responses` (`src/index.js:225`). Refresco 300 000 ms (`src/index.js:30`).
- **Multicuenta/cuotas**: ninguno. **Riesgo**: bajo; solo habla con el CPA local. Cabecera centinela de autorización (`src/index.js:16-17`).
- **Veredicto**: referencia de diseño; **no copiar código** (sin licencia).

### 2.2 ChadCSong/dsh-cliproxyapi-provider (TypeScript, MIT)
- **Descubrimiento**: `src/discovery.ts:175` usa `/v1/models?client_version=pi`; prueba en orden `baseURL`, variable de entorno, `127.0.0.1`, `localhost`, `[::1]` (`:1-5,110-123`); acota la respuesta a 4 MiB (`:157-167`) y rechaza redirecciones (`redirect: 'error'`, `:181`).
- **Enriquecimiento (solo CPA, sin tabla)**: cadenas de respaldo `context_window|max_context_window|context_length|contextWindow` y `max_output_tokens|max_tokens|maxTokens` (`:138-139`); oculta `visibility: hide` (`:132`); acepta `models` o `data` (`:48-54`). Un campo ausente **se omite** (`src/profile.ts:42-43`), no se inventa.
- **Debilidad**: si el modelo razona pero CPA no declara niveles, ofrece **todos** (`src/profile.ts:29-31`): sobre-declara capacidad.
- **Protocolos**: `openai-completions` (por defecto) o `openai-responses` (`src/config.ts:9,36`).
- **Fusión de tres vías**: `mergeRoute` (`src/runtime.ts:45-84`) conserva las ediciones del usuario sobre `name/contextWindow/maxTokens/reasoningEfforts` si no cambiaron respecto a lo generado antes; CPA manda siempre en `input`. Refresco 300 s, mínimo 15 s (`src/config.ts:6,37`). Mantiene el último estado bueno ante fallos (test `tests/runtime.spec.ts:6`).
- **Credenciales**: nombre de variable de entorno (`apiKeyEnv`, por defecto `CLIPROXYAPI_API_KEY`), no guarda la clave (`src/config.ts:35`). `scripts/harness-paths.mjs` solo lee paquetes de DSH y escribe en su propio repo.
- **Extra**: herramienta de generación de imágenes `/v1/images/generations` o Gemini `generateContent` (`src/image-request.ts:57`); el selector de modelos de visión se eliminó en 0.1.9 (CHANGELOG).
- **Pruebas**: 8+ archivos `tests/*.spec.ts` (descubrimiento, perfil, fusión, ciclo de vida, formulario, imagen). No se ejecutaron (sin `node_modules`).
- **Veredicto**: **mejor base licenciada y limpia** para el adaptador. Multicuenta/cuotas: ninguno.

### 2.3 pd90506/dsh-cliproxyapi-provider (JS, MIT)
- **Alcance**: solo modelos `claude-*` (`src/discover.js:98,305-307`) más una ruta xAI. Descubre en `/v1/models?client_version=pi` (`src/discover.js:32`).
- **Precedencia de capacidad**: `CPA ?? tabla ?? defecto` (`src/catalog.js:256-257`), con defectos 200000 / 16384 (`:69,75`). `CLAUDE_CAPACITY` son 16 filas escritas a mano (`:188-204`).
- **Por qué la tabla pudre**: refleja `CPA:internal/registry/models/models.json` (p. ej. `claude-3-5-haiku-20241022` = 128000/8192, `claude[19]`), es decir, duplica datos de CPA, y **le faltan** ids que models.dev ya lista en `providers/anthropic/models/` (`claude-opus-5-5`, `claude-sonnet-5-5`, `claude-haiku-5-5`, `claude-fable-5-2`), que caerían a 200000/16384.
- **Razonamiento (lo mejor del repo)**: usa el camino nativo de Anthropic, `thinking:{type:"adaptive"}` + `output_config.effort` (`src/catalog.js:80-127`); el conjunto ofrecido sale de la lista que publica CPA para ese modelo (CPA devuelve 400 si se pide un nivel no soportado, p. ej. `xhigh` en `claude-opus-4-6`, `:89-91`); nunca ofrece `minimal`, `ultra`, `none` (`:112`); sin lista, solo `off/low/medium/high` (`:97-102`).
- **Fallo confirmado (deriva)**: si CPA no responde, el refresco sobrescribe los modelos vivos con la instantánea empaquetada (Claude) o con `[]` (xAI) (`src/index.js:120-127,147-160`; `src/seed.js:53` refresco cada 6 h). Reproducido: 3→16 y 1→0 modelos con estado `refreshed` (`/tmp/cpa-prov/analysis/repro-drift.mjs`); el test `test/seed.test.mjs:936` inyecta un `throw` y no lo detecta. 38 tests pasan.
- **Veredicto**: copiar (con atribución) solo la **política de esfuerzos**; descartar tabla y lógica de deriva.

### 2.4 Factory48/dsh-cliproxyapi-provider
Continuación lineal del oficial (`9eb79fc → 9d552cf → 9d2d304`): puerto a DSH 0.2 (`settings.describe`, `currentConfig()`, reenvío de señal, `validateConfig`, `ui-plugin-manager`). No añade protocolos ni metadatos; mismo nombre de paquete. **Sin LICENSE.** Veredicto: ignorar.

### 2.5 lingyired/dsh-llm-cliproxyapi
`index.js` son 13 líneas deliberadamente vacías; todo el valor es `cordis.patch.yml` (179 líneas) que declara rutas `llm-pi-ai`: `token-unlimited` (relé de terceros), `cliproxyapi` (WorkBuddy), `cliproxyapi-codex` (Codex sobre OAuth de ChatGPT, Responses) y `cliproxyapi-trae`. **Sin LICENSE.** Riesgo **alto**: comparte credenciales de cuentas de consumo y rota cuentas personales (políticas de uso individual de OpenAI; ToS de WorkBuddy/Trae sin verificar), y envía prompts a un relé de origen desconocido. **Descartar.** Solo aprovechable el patrón (separar responses/completions y declarar `reasoningEfforts` explícitos).

### 2.6 @gray0128/dsh-cliproxyapi-provider 0.0.6
- **Sin LICENSE** en el tarball (MIT solo en `package.json`). Solo Responses.
- **`src/metadata.js` (911 líneas)**: calibra ventana, salida y niveles con pi-ai local, OpenRouter (`https://openrouter.ai/api/v1/models`) y LiteLLM (`raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json`) (`:70-71`), caché 1 h (`:73`), tope 12 MB, 8 s, sin credenciales, falla en abierto (scout).
- **Idea valiosa**: detecta **valores centinela** que CPA inventa (`PLACEHOLDER_CONTEXT_WINDOWS = {500000, 872000}`, `:7,90-94`) y plantillas Codex sintéticas (`:615-638`); una declaración que no sea centinela **siempre gana** (`:662-664,672-674`); casa ids con claves candidatas (`:177,516`).
- **Veredicto**: la mejor idea de enriquecimiento, pero **sin licencia**: reimplementar desde la descripción, no copiar.

### 2.7 @thebtf/dsh-cliproxyapi-provider 0.5.6
- **Licencia**: LICENSE MIT (Kirill Turanskiy) sobre código que comparte centinelas con el oficial sin licencia y sin atribución: una licencia nueva no sana la falta de la original. Requiere confirmación de procedencia antes de copiar.
- **Protocolos (4)**: `openai-responses`, `openai-completions`, `anthropic-messages`, `google-generative-ai`, con la ruta base de cada uno (`src/endpoint.js:12-16`; adaptadores `src/transport.js:8-11`).
- **Procedencia por campo**: cada límite se etiqueta `override | cpa | default` (`src/catalog.js:98-118`), con la cadena `max_context_window ?? context_window ?? context_length` (`:81-82`).
- **Resto**: migración de espacio de nombres en fases con rollback (`src/migration.js`, 132 líneas), GUI con locales en/ru/zh, límites desde el catálogo de pi-ai instalado (sin descarga remota).
- **Veredicto**: aprender (protocolo por modelo, procedencia); no copiar sin aclarar licencia.

### 2.8 dsh-cliproxyapi-pro 1.12.1
- **Licencia**: LICENSE MIT, titular "bowen"; repo TinyBOOOOM; 15 versiones del 2026-10-19 al 2026-11-17; `lib/client.js` (8 709 líneas) es la GUI.
- **Descubrimiento**: lista simple `/v1/models` y el catálogo `?client_version=pi` (`lib/host/discovery.js:207,265`); `lib/host/runtime.js:92` distingue si pudo leer el listado simple.
- **Enriquecimiento**: hechos opcionales de models.dev (`family, released, status, context, output, costOutput`) y OpenRouter (`rank, released, expires, context, intelligence`), TTL 24 h, tope 32/16 MiB (`lib/host/model-info.js:14-17,33,75`). Con ello recomienda modelos y marca retirados o líneas obsoletas (`STALE_LINE_DAYS = 180`, `lib/host/recommend.js:17,30`).
- **Cuotas por credencial**: lista credenciales con `GET /v0/management/auth-files` (`lib/quota/controller.js:206`) y consulta a cada proveedor mediante `POST /v0/management/api-call` (`lib/quota/management.js:180-182`) contra endpoints **internos no documentados**: `chatgpt.com/backend-api/wham/usage`, `api.anthropic.com/api/oauth/usage`, `cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota` y `fetchAvailableModels` (`lib/quota/constants.js:11-28`). Son credenciales del propio usuario, no un pool de terceros, pero dependen de APIs que pueden cambiar sin aviso. CPA ya trae `/quota/fetch` (`CPA:internal/api/server_management.go:84-86`).
- **Protocolo**: desde 1.12.1 manda `gemini-*` por Gemini nativo (`google-generative-ai`, ruta `<ruta>-gemini` sobre `<CPA>/v1beta`) para evitar la conversión Chat↔Gemini y conservar firmas de razonamiento (CHANGELOG 1.12.1).
- **Pendiente**: selección de cuenta/multicuenta y el resto de `lib/host/apply/*`.
- **Veredicto**: **aprender**: recomendación y ciclo de vida a partir de models.dev, y qué endpoints de cuota existen. No portar sondas (CPA ya las tiene) ni la GUI.

### 2.9 dsh-model 0.38.6
- **Licencia**: LICENSE MIT, titular "AIcivilization"; 52 versiones; actualizado hoy.
- **Qué es**: paquete "todo en uno" que **embebe CPA 8.0.13 sin modificar** (descarga el binario de la release con sha256, `engine-manifest.json:1-10`) y le suma tres cosas: (a) "check-in" diario de créditos en 12 productos (README; p. ej. Huawei CodeArts, `dist/checkin/ext/providers/codearts.js:1-9`, reclama campañas con `POST /v1/ops/claim`); (b) un **puente WorkBuddy** que descifra los archivos de autenticación cifrados de las apps de escritorio (`dist/bridge/workbuddy/desktop-credential-protection.js:1-9`, portado de `corrinehu/dsh-workbuddy-connect`); (c) un **pool gratuito** que reparte una clave sobre las cuotas gratuitas oficiales de muchos proveedores (`dist/free/service.js:1-5`, rutas `free/auto|smart|fast`). Contacta unos 118 hosts distintos (consolas de Baidu, Tencent, Huawei, Doubao, etc.).
- **Riesgo**: **alto** por (a) y (b): reclamar créditos automáticamente y leer credenciales de otra app de escritorio. Los ToS de cada proveedor no se verificaron.
- **Reutilizable**: `free-catalog.json` (`version`, `updatedAt`, `entries[]` con `kind`, `source`, `cycle`) como idea de esquema de "cuota gratuita"; los datos solo tras revisar procedencia y términos. Lo demás **descartar**.

### 2.10 pi-cliproxyapi-provider 0.16.24 (0xRichardH)
- **Licencia**: LICENSE MIT, titular "Richard Hao"; 111 versiones desde 2026-07-11; es el paquete más activo y el que más se acerca al estándar de catálogo del propietario.
- **Descubrimiento**: `GET /v1/models` con `data[]` obligatorio (`src/cpa.ts:47,59`), sin usar el catálogo Codex.
- **Enriquecimiento**: descarga `https://models.dev/api.json` (`src/models-dev.ts:5,44`), caché en disco (`src/discovery.ts:12`) y **copia de respaldo empaquetada** de 7.0 MB (`data/models-dev-fallback.json`).
- **Emparejamiento CPA→models.dev en cascada** (`src/matching.ts:19,123-176`): alias del usuario → exacto → prefijo de propietario (`owned_by`) → pista de propietario → sufijo → sufijo normalizado → proveedor de respaldo; los no emparejados se informan con una sugerencia de alias (`src/commands.ts:202-203`).
- **Razonamiento**: toma `reasoning_options` de models.dev siguiendo `canonical_model_id` (`src/model-capabilities.ts:86-104`) y añade reglas escritas a mano por familia GPT (`:106+`).
- **Detalle de contexto**: ofrece "canonical = 272000" o "full = límite de models.dev" para la ventana de GPT (`src/commands.ts:106`, `src/model-ui.ts:146`): evidencia del origen del 272K que los gateways rellenan.
- **Pendiente**: protocolos usados y cuotas/uso.
- **Veredicto**: **copiable (MIT) y el mejor modelo del emparejamiento**; en nuestro diseño esa lógica vive en el CATALOG (Go), el adaptador solo consume.

### 2.11 @router-for-me/pi-cliproxyapi-provider 1.5.0
- **Licencia**: LICENSE MIT, "Router-For.ME"; 36 versiones desde 2026-07-14.
- **Descubrimiento**: siempre `{raíz}/v1/models?client_version=pi` (`extensions/lib.ts:192,222-223`); defectos 128000 / 16384 (`lib.ts:35-36`); caché local `cliproxyapi-models.json` (`lib.ts:25`: es el archivo que veíamos en `~/.omp/agent/`, no una adición de izhimu).
- **Coste**: único candidato que enriquece **coste** desde models.dev, con tramos por contexto largo (`cost.tiers`, `lib.ts:703-738`), coste de modo rápido (`experimental.modes.fast.cost`, `lib.ts:883`) y emparejamiento por id (`lib.ts:927`).
- **Extras**: `codex-stream.ts` (~1.5K líneas), `auto-compact.ts`, `tps.ts`, `fast-footer.ts`, `pause.ts`.
- **Pendiente**: contenido de `codex-stream.ts` y `auto-compact.ts`.
- **Veredicto**: copiable (MIT); aprender el esquema de coste con tramos.

### 2.12 @izhimu/pi-cliproxyapi-provider 1.5.0-1
Fork del #11 que conserva su LICENSE. Diferencias medidas con `difflib` sobre `extensions/`: `lib.ts` +165/−48, `index.ts` +98/−30, `retry.ts` +85/−1, `codex-stream.ts` +62/−2, `auto-compact.ts` +38/−9, `tps.ts` +24/−2, `fast-footer.ts` +7/−2, y un archivo nuevo `omp-ambient.d.ts`. Añade adaptación a omp: directorio "contraparte" pi↔omp (`getCounterpartAgentDir`), resolución de identidad, conexión y valores por defecto (`resolveIdentity`, `resolveConnection`, `resolveFastDefault`, `resolvePauseDefault`) y una caché de coste en memoria. Es el puente histórico de omp; no aporta metadatos nuevos. Veredicto: **referencia de adaptador omp**, a revisar si el paquete propio incluye omp.

## 3. Matriz de funciones

Leyenda de destino: **CORE** (fork Go de CPA), **CATALOG** (normalizador), **FRONT** (Management Center), **HOST** (adaptador omp/pi/dsh), **DROP**.
"Copiable" = licencia efectiva permite copiar código (MIT con LICENSE); si no, solo reimplementar desde la idea.

| Función | Mejor implementación (repo:archivo) | Destino | Copiable | Nota |
|---|---|---|---|---|
| Entregar límites, niveles de razonamiento y modalidades de forma estable en la API pública | CPA `internal/client/codex/models/models.go:546-553,855-901` (ya existe) + hueco: `/v1/models` los filtra (`openai_handlers.go:85-109`) | **CORE** | sí (CPA es MIT) | exponer registro completo con procedencia; hoy los providers dependen de `?client_version=` |
| Detección de límites centinela inventados por CPA (500000, 872000, plantillas Codex) | @gray0128 `src/metadata.js:7,90-94,615-638,650-690` | **CATALOG** | no | regla: una declaración real nunca se pisa; centinela → buscar en fuentes |
| Enriquecimiento externo (límites, modalidades, esfuerzos) desde models.dev | pi-cliproxyapi-provider `src/models-dev.ts:5,44`, `src/model-capabilities.ts:86-104`; (@gray0128 usa OpenRouter/LiteLLM/pi-ai, `src/metadata.js:70-71`) | **CATALOG** | #10 sí (MIT); gray no | fuentes del estándar: models.dev + modelparams.dev; no OpenRouter/LiteLLM |
| Emparejamiento de ids CPA→models.dev en cascada (alias, exacto, `owned_by`, sufijo, sufijo normalizado, proveedor de respaldo) + informe de no emparejados con alias sugerido | pi-cliproxyapi-provider `src/matching.ts:19,123-176`, `src/commands.ts:202-203` | **CATALOG** (reimplementar en Go; usar `canonical_model_id`) | sí (MIT, "Richard Hao") | es la pieza más reutilizable como especificación |
| Coste de lista con tramos por contexto largo y modo rápido | @router-for-me/pi `extensions/lib.ts:151-155,703-738,881-883,927` | **CATALOG** (models.dev `cost`; CPA no tiene campo de coste) | sí (MIT, "Router-For.ME") | cubre el hueco "coste `unknown`" de `design.md` |
| Instantánea de respaldo de models.dev (7.0 MB) | pi-cliproxyapi-provider `data/models-dev-fallback.json` | **CATALOG** (versionada con el catálogo, no dentro del paquete del host) | sí | evita descargar al arrancar |
| Recomendación y ciclo de vida (retirado, línea obsoleta a 180 días) | dsh-cliproxyapi-pro `lib/host/recommend.js:17,30`, `lib/host/model-info.js:17,33` | **CATALOG** (`status` de models.dev) + **FRONT** | sí (MIT, "bowen") | una sola vez en el normalizador, no por host |
| Reglas de razonamiento por familia GPT escritas a mano | pi-cliproxyapi-provider `src/model-capabilities.ts:106+` | **DROP** | n/a | tabla que pudre; usar `reasoning_options`/modelparams.dev |
| Procedencia por campo (`override`/`cpa`/`default`) | @thebtf `src/catalog.js:98-118` | **CATALOG** (campo `provenance`) | dudoso | encaja con `catalog-proof.md`; reimplementar |
| Precedencia `CPA ?? tabla ?? defecto` con `unknown` explícito | pd90506 `src/catalog.js:249-262` (precedencia); **sin** sus defaults ni tabla | **CATALOG** | sí | defecto debe ser `unknown`, no 200000/16384 ni 262144/32768 |
| Conjunto de esfuerzos por modelo desde la lista de CPA; nunca `minimal/ultra/none`; adaptive thinking | pd90506 `src/catalog.js:80-127` | **CATALOG** (enums de modelparams.dev) + **CORE** (validar/traducir) + **HOST** (mapear a perfil) | sí | no sobre-declarar (ChadCSong `profile.ts:29-31` lo hace) |
| Fusión de tres vías que respeta ediciones del usuario | ChadCSong `src/runtime.ts:45-84` | **HOST** (y `overrides.json` del pipeline de sincronización) | sí (MIT) | encaja con la rebanada 4 de `design.md` |
| Fetch acotado (4 MiB), sin redirecciones, candidatos locales, último estado bueno | ChadCSong `src/discovery.ts:1-5,157-200`; `src/runtime.ts` | **HOST** | sí | en Go el CORE ya tiene su propio cliente; copiar solo para el adaptador TS |
| Selección de protocolo por modelo (responses / completions / anthropic / gemini) | @thebtf `src/endpoint.js:12-16`, `src/transport.js:8-11` | **HOST**, decidido por `native_capabilities` del CATALOG | dudoso | hoy ChadCSong solo ofrece 2; pd90506 fuerza Anthropic |
| Cuotas por credencial y reinicio | CPA `server_management.go:84-86` (ya existe); sondas de dsh-cliproxyapi-pro vía `POST /v0/management/api-call` a endpoints internos (`lib/quota/constants.js:11-28`, `management.js:180-182`) | **CORE** (existe) + **FRONT** (mostrar) | n/a | no reimplementar; las sondas dependen de APIs no documentadas: solo aprender qué endpoints cubrir |
| Enrutado y rotación multicuenta | CPA `server_management.go:131-133` (ya existe) | **CORE** | n/a | no duplicar |
| Herramienta de imágenes | ChadCSong `src/image-request.ts:57` | **HOST** opcional | sí | fuera del primer paquete |
| Migración de espacio de nombres con rollback | @thebtf `src/migration.js` | **DROP** | no | específico de DSH y puntual |
| Formularios/tarjetas de configuración | ChadCSong `src/client/*`, @thebtf `client.js` | **DROP** (la UI es del FRONT) | n/a | el adaptador solo necesita `baseURL`, `apiKeyEnv`, `protocol` |
| Pool de cuotas gratuitas, check-in de créditos, puente WorkBuddy | dsh-model `dist/free/service.js:1-5`, `dist/checkin/ext/providers/codearts.js:1-9`, `dist/bridge/workbuddy/desktop-credential-protection.js:1-9` | **DROP** | n/a | riesgo alto (reclamo automático de créditos, descifrado de credenciales de otra app); su `free-catalog.json` solo como esquema |
| Rutas de relé de terceros y OAuth de ChatGPT compartido | lingyired `cordis.patch.yml` | **DROP** | n/a | riesgo alto |
| Política de deriva a 6 h con instantánea empaquetada | pd90506 `src/seed.js:53`, `src/index.js:112-160` | **DROP** | n/a | bug confirmado (§2.3) |
| Extensiones pi: Codex stream, auto-compact, retry, tps, pausa | @router-for-me/pi `extensions/*`; @izhimu añade `retry.ts` y adaptación a omp | **HOST** (pi/omp) | sí (MIT, "Router-For.ME") | utilidades de UX del host, no del gateway; contenido de `codex-stream.ts`/`auto-compact.ts` sin leer |
| Adaptación pi↔omp (directorio contraparte, identidad, conexión) | @izhimu `extensions/lib.ts` (`getCounterpartAgentDir`, `resolveConnection`), `omp-ambient.d.ts` | **HOST** (omp) | sí | referencia si el paquete propio cubre omp |
| Gemini nativo (`<ruta>-gemini` sobre `/v1beta`) | dsh-cliproxyapi-pro CHANGELOG 1.12.1 | **HOST** (parte de la selección de protocolo) | sí | conserva firmas de razonamiento; evita conversión en CPA |

## 4. Recomendación

1. **Base del adaptador de host: ChadCSong** (MIT con LICENSE, TypeScript, pruebas, no inventa límites, fusión de tres vías). Copiable con aviso de copyright. Cambios: ofrecer los 4 protocolos según el catálogo, no ofrecer niveles que CPA no declara (`profile.ts:29-31`), y mover la decisión de límites al CATALOG.
2. **Metadatos desde models.dev**: la lógica de emparejamiento y de esfuerzos de `pi-cliproxyapi-provider` (MIT) es el mejor punto de partida **como especificación para el CATALOG en Go** (`matching.ts:123-176`, `model-capabilities.ts:86-104`), y el esquema de coste con tramos de `@router-for-me/pi-cliproxyapi-provider` (`lib.ts:703-738`) cubre el coste que CPA no tiene. Se copia con aviso de copyright o se reimplementa; el adaptador de host no debe llevar esa lógica.
3. **Aprovechar de pd90506** (MIT, con atribución) solo la **política de esfuerzos** y el mapeo Anthropic adaptive (`catalog.js:80-127`). No copiar `CLAUDE_CAPACITY`, el relleno 200000/16384 ni el refresco de deriva (bug).
4. **Solo aprender, sin copiar código**: oficial `router-for-me/dsh-cliproxyapi-provider`, Factory48, @gray0128 y lingyired (sin LICENSE), y **@thebtf** (LICENSE propia pero esqueleto derivado sin licencia ni atribución). De @gray0128 la detección de centinelas; de @thebtf la procedencia por campo y la selección de protocolo.
5. **Lo importante va al CORE y al CATALOG, no a un provider**: exponer en CPA un catálogo público completo (límites, niveles, modalidades, coste desconocido explícito, procedencia) para que ningún provider tenga que usar `?client_version=` ni tablas propias; hoy `/v1/models` filtra campos que el registro sí posee (`openai_handlers.go:85-109`). Las cuotas y el enrutado multicuenta ya existen en CPA (§0): no portarlos.
6. **Descartar**: lingyired (relé de terceros y cuentas compartidas), **dsh-model** (check-in automático de créditos y descifrado de credenciales de WorkBuddy; su único activo, `free-catalog.json`, solo como idea de esquema), las sondas de cuota de dsh-cliproxyapi-pro (CPA ya tiene `/quota/*`; aprender de él solo la recomendación/ciclo de vida), la GUI de los providers DSH (la UI es el Management Center), la migración de @thebtf.
7. **Restricciones de licencia**: `"license": "MIT"` solo en `package.json` (oficial, Factory48, @gray0128, lingyired) no da texto ni titular: tratar como "sin licencia" hasta que el autor publique un LICENSE. MIT exige conservar aviso y texto en copias (ChadCSong, pd90506, y los de la familia pi con "Router-For.ME"). Los datos de router-for-me/models siguen sin LICENSE (ver `sources.md`): solo como dato factual con atribución.

## 5. Siguientes pasos (si se aprueba)
- Convertir `matching.ts` de #10 en especificación del normalizador y probarla contra los modelos reales de `models.dev/providers/*` (cuántos ids de CPA quedan sin emparejar).
- Prototipar en CATALOG la regla "centinela → fuente externa → `unknown`" con las cifras de `catalog-proof.md` (centinelas observados: 500000 y 872000; defectos 262144/32768 y 200000/16384 de estos providers).
- Pedir LICENSE a router-for-me si interesa reutilizar el oficial.

## 6. Pendiente / sin verificar
- **Lectura parcial** de 8–12: se verificó descubrimiento, enriquecimiento, licencia y riesgos con `grep`/lectura dirigida; no se leyó entero `lib/client.js` (8 709 líneas, GUI) ni `lib/host/apply/*` de dsh-cliproxyapi-pro, ni la selección de cuenta de su multicuenta; ni `codex-stream.ts`/`auto-compact.ts` de la familia pi; ni los protocolos que #10 delega a pi-ai.
- Los dos subagentes de lectura de los grandes paquetes npm y de la familia pi no habían entregado su informe al cerrar; los datos de 8–12 son de lectura directa mía y no incluyen su análisis.
- Pruebas de ChadCSong no ejecutadas (sin `node_modules`); comportamiento de red no probado contra un CPA vivo.
- Términos de uso (OpenAI, WorkBuddy, Trae, Huawei CodeArts, relé `token-unlimited.com`): citados desde el análisis de subagentes o inferidos del código, no leídos en el texto oficial.
- Que el catálogo Codex de CPA entregue exactamente los campos de `models.go` para todos los canales: visto en código, no contra una instancia viva.
- Artefactos de diff de los subagentes en `/tmp/cpa-analysis/` y `/tmp/cpa-prov/analysis/`; ningún repo candidato ni de `~/repositories` fue modificado.
