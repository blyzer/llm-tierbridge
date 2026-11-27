# freellmapi dentro de CPA (CLIProxyAPI): mapeo, archivos calientes, front, catálogo y fork

Fecha: 2026-11-26. Estado: **investigación, incompleta** (la ejecución se cortó a ~9 min de un tope de 15). Lo no verificado
va marcado **pendiente**. Complementa `design.md`, `sources.md`, `catalog-proof.md` y `usage-and-language.md` (mismo directorio).
"CPA" = `router-for-me/CLIProxyAPI` v8.1.4 (Go 1.26, `~/repositories/CLIProxyAPI`). freellmapi = `tashfeenahmed/freellmapi`
v0.16.44 (TS, `~/repositories/freellmapi`). Front = `Cli-Proxy-API-Management-Center` (MC) v1.29.2.

## 0. Conclusión

1. **No forkear CPA.** CPA ya cubre ~70 % de freellmapi por configuración: los proveedores OpenAI-compatibles de freellmapi
   (36 declarativos + 37 subclases = 73 de 104 registros) caben en `upstream.api-keys.openai-compatibility[]`
   (`name`, `base-url`, `headers`, `keys[]` con `weight`, `models[]` con `alias`, pools de alias con failover).
2. **El catálogo no entra por `models.catalog`.** Esa fuente solo rellena secciones fijas (`claude`, `gemini`, `codex-*`, `kimi`…);
   un canal nuevo exige cambiar Go. La vía es **generar el bloque `openai-compatibility`** desde datos de freellmapi (§4).
3. **Lo que CPA no tiene** (cuotas por modelo rpm/rpd/tpm/tpd, cadena ordenada por `intelligence_rank`, proveedores nativos no
   OpenAI) se cubre, si hace falta, con un **plugin** (C ABI: `Scheduler`, `QuotaProvider`, `ProviderExecutor`, `ManagementAPI`),
   no con un fork. Eso es la rebanada 6 de `design.md` y requiere ADR.
4. **Front:** usar el asset de release de MC; las vistas propias van como página de plugin (iframe en `/plugin-pages/:id/:n`).
5. **Dato abierto limitado:** el registro MIT dentro de freellmapi son **108 modelos de chat en 15 plataformas** (semilla V25);
   el resto vive en un feed firmado de pago (§4).

## 1. Tabla de mapeo freellmapi → CPA

| # | freellmapi | Evidencia | Equivalente CPA | Brecha | Estrategia |
|---|---|---|---|---|---|
| 1 | Proveedor OpenAI-compat: 36 `register(new OpenAICompatProvider({platform,name,baseUrl,…}))` + 37 subclases (`airforce`, `apinex`, `blaze`, `clod`…) | `server/src/providers/index.ts` (104 `register`), `openai-compat.ts` | `openai-compatibility[]`; ejecutor `internal/runtime/executor/openai_compat_executor.go` | Las 37 subclases pueden llevar quirks (cabeceras, parámetros, timeouts): **pendiente** auditar una a una | **Generar config** (§4) |
| 2 | Proveedores nativos: `google`, `cohere`, `cloudflare`, `aihorde`, `puter`, `sail` (Responses), `zhipu`, `moondream`, `jamai`, `aclide` (17 subclases directas de `BaseProvider`, incluida la base compat) | `extends BaseProvider` | `gemini`/`aistudio` ya existen en CPA; el resto no | Sin ejecutor para Cohere, Cloudflare, AI Horde, Puter, Sail | Plugin `ProviderExecutor` solo si se mide necesidad; Cohere ❌ por ToS: no portar |
| 3 | Búsqueda (10 `SearchOnlyProvider`), voz (`NativeSpeechProvider` 2 + TTS/STT), decisión (4 `KeylessDecisionProvider`), imagen/vídeo | `providers/*.ts`, `media-*.ts` | Sección `multimedia` en config (no leída) | **pendiente** | Fuera de alcance v1 |
| 4 | Cadena de fallback: `fallback_config` (108 filas), `intelligence_rank`, `speed_rank`, `scoring.ts`, `model-groups.ts` | BD migrada | Pool de alias repetido = round-robin + continúa con el siguiente upstream si falla antes de producir salida; `routing.strategy` = `round-robin`/`weighted-round-robin`/`fill-first` (global); `priority` por clave | CPA no ordena por rango de modelo; `fill-first` es global, no por alias | Orden por `priority` de clave + pools; resto vía plugin `Scheduler` |
| 5 | Límites por modelo `rpm/rpd/tpm/tpd` (cobertura: 76/55/28/12 de 108), `monthly_token_budget` (108/108), `quota-forecast`, `provider-quota` | `models`, `services/ratelimit.ts` | No vi campos de cuota por modelo en `openai-compatibility`; sí `weight`, `disable-cooling`, `request-retry`, `request-scoped-errors` | Sin contabilidad proactiva de cuota | Plugin `QuotaProvider`/`Scheduler` (semántica **pendiente** de leer en `sdk/pluginapi/types.go`) |
| 6 | Cooldown por 429 con `Retry-After` acotado a 24 h | `providers/base.ts` (`parseRetryAfterMs`, `MAX_RETRY_AFTER_MS`) | `sdk/cliproxy/auth/conductor_cooldown.go`; acciones `continue-and-cooldown`/`stop-and-cooldown` en `request-scoped-errors` | Si CPA respeta `Retry-After`: **pendiente** | Cubierto parcialmente |
| 7 | Bóveda de claves cifrada en SQLite, presupuesto por clave (`key-budget.ts`) | `lib/crypto.ts`, `api_keys` | Claves en config/`auths/`; stores Postgres/git/objeto | Cifrado en reposo no verificado | No importar la bóveda; claves por variable de entorno |
| 8 | Clave unificada del cliente | `getUnifiedApiKey` | `access.api-keys` | Ninguna | Directo |
| 9 | Registro de modelos (`models`: plataforma, `model_id`, límites, `context_window`, `supports_vision/tools`) | `db/migrations/*`, dump propio | `models[].name/alias/display-name/max-context-length/input-modalities` en `openai-compatibility` | Límites rpm/rpd no representables | §4 |
| 10 | Coste equivalente `paid_input_per_m/paid_output_per_m` (100/108) | `db/model-pricing.ts` | Campos de `ModelInfo` del plugin **pendiente** de leer | **pendiente** | Guardar en el catálogo propio |
| 11 | Quirks (11 filas, severidad `blocker/warning/info`, destino plataforma+glob) | tabla `quirks` | `excluded-models`, `request-scoped-errors` | Los avisos informativos no existen | Mostrar en el front propio |
| 12 | Superficies Anthropic/Gemini/Ollama, Responses | `routes/anthropic.ts`, `gemini.ts`, `ollama.ts`, `responses.ts` | Traductores `internal/translator` (OpenAI/Gemini/Claude/Codex) | Emulación Ollama: no vista | Sin acción |
| 13 | Perfiles, `client-classifier`, `context-handoff`, `compression`, `fusion`, backups, OIDC | `services/*` | Sin equivalente | n/a | No portar |
| 14 | Feed de catálogo firmado (Ed25519), Premium | `services/catalog-sync.ts` | `models.catalog` (fuente propia) | Legal/ToS | No consumir el feed vivo (§4) |

## 2. Archivos calientes

Método: `git log --since=2026-10-26 --no-merges --name-only`, ficheros `.go/.yaml/.json`, pruebas excluidas.

**CPA (587 commits en el periodo).** Riesgo de conflicto si se modifica el motor:

| Cambios | Archivo | Por qué importa aquí |
|---|---|---|
| 44 / 29 / 27 / 25 / 23 / 22 | `sdk/cliproxy/auth/conductor_execution.go`, `_stream`, `_home`, `_cooldown`, `_selection`, `_home_execution` | Aquí viviría cualquier cuota o cadena propia: **zona roja** |
| 38 | `config.example.yaml` | Cualquier clave nueva choca |
| 20 | `internal/config/config_types.go` | Idem para el esquema |
| 17 | `internal/runtime/executor/openai_compat_executor.go` | Ejecutor que ya usan los 73 proveedores compat |
| 15 | `sdk/pluginapi/types.go` | **Riesgo ABI del plugin**: fijar versión de CPA |
| 13 | `internal/registry/models/models.json` | Catálogo embebido; cambia casi cada 2 días |
| 12 | `internal/pluginhost/adapters_executors.go` | Adaptador de ejecutores de plugin |
| 26–18 | `internal/runtime/executor/antigravity_*`, `codex_*` | Calientes pero ajenos a freellmapi |

Frío/seguro: directorios nuevos (`internal/<nuevo>/`, o un plugin fuera del árbol). `AGENTS.md` de CPA añade fricción: no usar `encoding/json`
ni `regexp` estándar (usar `github.com/MoeForge/moego/*`), ayudas de ejecutor solo en `internal/runtime/executor/helps/`, y no tocar
`internal/translator/` aislado.

**freellmapi (171 commits).** Qué leer del upstream y cuánto cambia:

| Cambios | Archivo | Uso para el adaptador |
|---|---|---|
| 33 | `server/src/routes/keys.ts` | Tablas por plataforma (no parsear) |
| 27 | `shared/types.ts` | Unión `Platform` (105 miembros): **fuente de ids** |
| 22 | `server/src/providers/index.ts` | Registros declarativos: **fuente de `baseUrl`/`name`** (regex ya extrae 36 bloques) |
| 26 / 19 | `client/src/components/keys/shared.tsx`, `client/src/lib/routing.ts` | Solo front |
| ~16 c/u | `client/src/i18n/locales/*.json` (decenas de idiomas) | Ruido; ignorar |

Un commit de alta de proveedores (`7c9a3805`) toca `shared/types.ts`, `providers/index.ts`, `routes/keys.ts`, `keys/shared.tsx`,
`services/{decision,embeddings,keyless-defaults,specialist-text}.ts`. El adaptador debe fallar en alto si la forma de
`register(new OpenAICompatProvider({...}))` cambia.

## 3. Mapeo del front

MC (`src/router/MainRoutes.tsx`): `/dashboard`, `/ai-providers` (workbench), `/auth-files`, `/oauth`, `/routing`, `/quota`, `/plugins`,
`/plugin-pages/:pluginId/:menuIndex` (**iframe** de un recurso del plugin, `PluginResourcePage.tsx`), `/config`, `/logs`, `/system`.

| freellmapi (`client/src/App.tsx`) | MC | Brecha / acción |
|---|---|---|
| `/models/chat` (FallbackPage, cadena ordenada) | `/routing` + `/ai-providers` | Sin cadena por rango: página de plugin |
| `/models/chat/:id` (ModelDetail) | — | Página de plugin (límites, coste, quirks) |
| `/keys` (KeysPage) | `/ai-providers` (grupos de claves, `providers.ts`) | Cubierto; falta presupuesto por clave y proveedores sin clave |
| `/analytics`, `/logs` | `/dashboard`, `/logs`, `/quota` | Cubierto en lo básico |
| `/models/{embeddings,image,video,audio,search,decision,fusion}` | — | Fuera de alcance v1 |
| `/playground`, `/agents` | — | No portar (EasyCLIProxyAPI cubre agentes) |
| `/premium` | — | No portar |

Decisión: **no forkear MC** (124 commits en 30 días sobre una SPA). Un icono de marca "freellm" en `ProvidersWorkbenchPage`
(`descriptors.ts`, `brandLogos.ts`) exigiría parche: **pendiente** medir. Las vistas propias salen como recurso de plugin
(`ManagementAPI`). Nota: `AGENTS.md` de CPA declara `/v0/management` obsoleto y existe `docs/management-api-v8.md`: **pendiente** leerlo
y confirmar qué endpoint de configuración usa MC para editar `openai-compatibility`.

## 4. Diseño de ingesta del catálogo

**Qué hay realmente (medido).** Ejecuté las 56 migraciones de freellmapi con Bun en una BD temporal (`/tmp/fl-reg`): 108 modelos de
chat en 15 plataformas (`openrouter` 23, `nvidia` 13, `ollama` 11, `cloudflare` 10, `google` 9, `groq` 9, `mistral` 7, `opencode` 6,
`cerebras` 4, `cohere` 4, `kilo` 4, `zhipu` 3, `huggingface` 3, `pollinations` 1, `llm7` 1), 10 embeddings y 11 quirks, todos
`source='catalog'`. `db/README.md`: V25 fue la última migración con datos; desde entonces el catálogo lo entrega un feed firmado.
El README anuncia 736 familias y 1 313 endpoints gratuitos: el dato abierto es solo una semilla.

**Feed vivo.** `GET {api.freellmapi.co}/v1/latest`, clave pública Ed25519 fijada en `catalog-sync.ts`, dos veces al día; nivel `live`
(Premium, 19 USD/año) o `monthly` (los modelos llegan 30 días tarde). **Pendiente** términos del feed mensual; decisión vigente de
`design.md`: no consumirlo ni redistribuirlo.

**Por qué no `models.catalog` de CPA.** `staticModelsJSON` tiene secciones fijas y `validateModelsCatalog` solo exige id no vacío y sin
duplicados; una clave `freellmapi` se ignoraría. Además una fuente personalizada **reemplaza** a la oficial (no hay fallback), así que
habría que mantener el catálogo oficial fusionado. Sí sirve, más adelante, para corregir límites de modelos que CPA ya sirve.

**Pipeline (solo datos, sin LLM; sigue §3 de `design.md`):**

1. **Extraer** de un checkout fijado a tag: ids (`shared/types.ts`), `name`/`baseUrl`/cabeceras (`providers/index.ts`), modelos y límites
   (migraciones ejecutadas en BD temporal, o tabla `models` de una instalación propia con `source='catalog'`).
2. **Normalizar** a `gateway-model.schema.json` como `observation` con `source: freellmapi`, `file`, `commit/tag`, y una marca `tos`
   (del repaso de ToS de mayo 2026 del propio proyecto: Cohere ❌; Gemini, NVIDIA NIM, Z.ai, Cloudflare ⚠️). El esquema necesita un campo
   nuevo de límites de capa gratuita (`rpm/rpd/tpm/tpd`, `monthlyTokenBudget`): **pendiente** añadirlo.
3. **Fusionar** con la precedencia de `catalog-proof.md` §9 (models.dev gana en contexto y coste de lista; freellmapi es la única fuente de
   límites de capa gratuita).
4. **Emitir**: (a) fragmento YAML `openai-compatibility` por plataforma (`prefix: "free"`, `base-url`, `models[].name/alias`, claves
   como referencia a variable de entorno, nunca en git; las ❌/⚠️ con `disabled: true`); (b) `freellm-limits.json` para un plugin
   `Scheduler`/`QuotaProvider` (**pendiente**: consumidor aún no existe).
5. **Sincronizar**: diff clasificado (alta, retirada, límite, parámetro), PR revisable, `sync-state.json`; falla en alto si 0 bloques
   `register(...)` o si el número de plataformas cae más de un umbral.
6. **Aplicar**: CPA recarga la config en caliente (`internal/watcher`). **Pendiente** si admite incluir ficheros; si no, un script fusiona el
   fragmento en `config.yaml`.

**Límites conocidos:** el pool de alias de CPA es round-robin, no ordenado por `intelligence_rank`; cuotas por modelo no representables en
YAML; las 37 subclases pueden requerir cabeceras o parámetros que el YAML no expresa.

## 5. Recomendación de fork

| Opción | Veredicto | Motivo |
|---|---|---|
| A. Forkear CPA y portar proveedores a Go | **No** | 587 commits/30 días; los puntos de inserción (`conductor_*`, `config_types.go`) son los más calientes; reglas de JSON propias; 16 proveedores nativos que portar |
| B. Plugin C ABI (Go/C/Rust) | **Después, condicional** | Sin fork; trae `Scheduler`, `QuotaProvider`, `ProviderExecutor`, `ManagementAPI` con página en MC. Riesgo: `sdk/pluginapi/types.go` cambia (15 commits/30 días): fijar versión de CPA. **Pendiente** confirmar que el build local carga plugins |
| C. Config generada desde datos | **Primero** | Cubre ~70 % (compat) sin tocar código; reversible |
| D. Forkear freellmapi | **No** | Dato abierto = semilla de 108 modelos; el motor es TS (no embebible en Go); el feed vivo es de pago |

**Reconciliación:** `sources.md` fila 1 dice "Fork + remote upstream" para freellmapi; `design.md` §0 dice "solo datos". Esta
nota se alinea con `design.md`: **sin fork**, solo un checkout/remoto de solo lectura fijado a tag para el adaptador.

**Condición para forkear CPA:** si el plugin demuestra que falta un gancho en el conductor (p. ej. cuota por modelo antes de elegir
credencial), preferir un PR a upstream; solo si no se acepta, una pila mínima de parches limitada a `sdk/cliproxy/auth/`. Requiere ADR.

## 6. Pendiente

ToS del feed mensual y de los proveedores; semántica de `QuotaProvider`/`Scheduler` y campos de `ModelInfo` del plugin; `docs/management-api-v8.md`;
sección `multimedia`; si CPA acepta `include` de config; `Retry-After` en CPA; auditoría de las 37 subclases; modo Home (CLIProxyAPIHome);
coste de parchear la marca en MC; carga de plugins en el build local; extracción de los 108 modelos con ids exactos como prueba de
reproducibilidad (el volcado está en `/tmp/fl-reg/dump.json`, no versionado).
