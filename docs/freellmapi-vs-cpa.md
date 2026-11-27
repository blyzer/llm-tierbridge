# freellmapi frente a CLIProxyAPI: cómo funciona cada uno

Fuentes: `sources.md` §1–2, `freellmapi-into-cpa.md` §1 y `cpa-plugin-smoke.md`.
Clones leídos: `tashfeenahmed/freellmapi` (v0.16.44) y `router-for-me/CLIProxyAPI`
(v8.1.x).

## La diferencia de fondo

| | freellmapi | CLIProxyAPI (CPA) |
|---|---|---|
| Pregunta que resuelve | **Qué modelo gratuito usar ahora**, según calidad y cuota restante | **Cómo hablar con un proveedor y con qué credencial**, sea cual sea el formato del cliente |
| Unidad de decisión | El modelo, ordenado por `intelligence_rank` y `speed_rank` | La credencial o cuenta, en un pool con round-robin y cooldown |
| Origen de las credenciales | Claves de API del usuario y canales sin clave (Kilo, OVH, AI Horde) | Cuentas OAuth de CLIs (Claude Code, Codex, Gemini CLI, Antigravity, Kimi…), claves de API y `openai-compatibility` |
| Lenguaje y runtime | TypeScript/Node monorepo (`server`, `client`, `cli`, `desktop`), SQLite | Go, un solo binario; la lógica vive en `internal/`, lo embebible en `sdk/` |
| Superficie de API | OpenAI-compatible, más Anthropic, Gemini, Ollama y Responses | OpenAI (incl. Responses), Gemini, Claude y Codex, con traductores entre todos ellos |
| Catálogo | Tabla `models` en SQLite, creada por 56 migraciones (108 modelos de chat abiertos) más un feed firmado de pago | `models.json` embebido (212 modelos, 14 canales), refrescado cada 3 h desde `router-for-me/models` |
| Límites por modelo | rpm/rpd/tpm/tpd y presupuesto mensual de tokens | No tiene cuotas por modelo; registra la suspensión por cuota de cada credencial |
| Costes | Equivalente de pago por millón de tokens (100 de 108 modelos) | Ninguno |
| Thinking y esfuerzo | Sin campos propios, solo quirks | Niveles por modelo (`low…max`) y presupuestos `min`/`max` |
| Errores 429 | Cooldown con `Retry-After`, acotado a 24 h | Cooldown en `conductor_cooldown.go`, con acciones `continue`/`stop-and-cooldown` |
| Claves | Bóveda cifrada en SQLite, presupuesto por clave | En la config o en `auths/`; stores Postgres, git u objeto |
| Front | Dashboard propio (React) | Management Center aparte, sobre la API de gestión `/v0/management` |
| Extensión | Añadir un proveedor es una subclase TS más una migración | Plugins C ABI (`dlopen`, requiere cgo) con `Scheduler`, `QuotaProvider`, `ProviderExecutor` y `ManagementAPI` |
| Ritmo | 171 commits en 30 días | 587–658 commits en 30 días, releases casi diarios |

## Cómo fluye una petición

**freellmapi:**
1. El cliente pide `auto` o un modelo concreto.
2. El router recorre la cadena de fallback por ranking.
3. Descarta los modelos sin cuota restante y salta los que están en cooldown.
4. Toma una clave de la bóveda y llama al adaptador del proveedor (104 registros).
5. Si recibe un 429, aplica cooldown y prueba el siguiente modelo.

**CPA:**
1. El cliente llama en su propio formato (OpenAI, Claude, Gemini o Codex).
2. CPA resuelve el alias del modelo y su pool de credenciales.
3. El conductor elige una credencial (round-robin, prioridad, cooldown) y refresca el token OAuth si hace falta.
4. El traductor convierte la petición al formato del proveedor, y de vuelta la respuesta y el streaming.
5. Si falla antes de empezar el streaming, pasa a la siguiente credencial o al siguiente upstream.

## Qué toma nuestro producto de cada uno

- **De CPA, el motor:** traductores, pool de credenciales, thinking, API de gestión y plugins.
- **De freellmapi, la política de selección:** ranking, cuotas por modelo y presupuesto. Va como plugin (`Scheduler` y `QuotaProvider`), y sus datos abiertos alimentan el catálogo.
- **De models.dev, modelparams.dev y router-for-me/models:** el catálogo normalizado, con costes, límites y parámetros.

## Límites legales

- **freellmapi:** no se consume su feed Premium, y su README lo declara "para experimentación personal".
- **CPA:** sus canales OAuth reenvían suscripciones de consumo de terceros y no se han verificado sus ToS. Para un producto, usar solo los canales de clave de API.
