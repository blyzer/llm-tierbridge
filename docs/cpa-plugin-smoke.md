# CPA plugin smoke (macOS arm64) — 2026-11-26

## Veredicto
**Plugins cargan en esta máquina: SÍ.** Se necesita un binario CPA compilado con CGO (el que corre ahora ya lo es) y plugins `c-shared` `.dylib` darwin/arm64.

## 1. Binario local en ejecución
- PID 60879: `~/Library/Application Support/com.cpa.gui/cpa-core/cli-proxy-api -config .../cpa-core/config.yaml` (lanzado por `EasyCLIProxyAPI.app` / `cpa-gui`). No es brew.
- `go version -m`: `mod github.com/router-for-me/CLIProxyAPI/v8 v8.1.7+dirty`, `-X main.Version=8.1.7 Commit=47f18e4d`, **`CGO_ENABLED=1`**, `-buildmode=exe`, GOARM64=v8.0. Soporta plugins.
- Su config tiene `plugins.enabled: true`, `dir: "plugins"`. Ya hay ~39 `.dylib` en `cpa-core/plugins/darwin/arm64/` (quota-monitor, model-mapper, usage-audit, cpa-codex-weight-balancer, etc.) más un `.shadow/`. No se tocó nada de eso.
- Release workflow: solo `freebsd-arm64-no-plugin` está con `cgo_enabled: false` (documentado "no soporta plugins"). Windows usa `loader_windows.go` (DLL, sin dlopen/cgo).
- Un clon HEAD 38fe9295 no es la misma versión que el binario en ejecución (47f18e4d, v8.1.7).

## 2. Smoke
Comandos (todo en /tmp/cpa-smoke):
```
CGO_ENABLED=1 go build -o /tmp/cpa-smoke/cli-proxy-api ./cmd/server        # 97 s, OK
cd examples/plugin && make BIN_DIR=/tmp/cpa-smoke/bin /tmp/cpa-smoke/bin/management-api-go.dylib
   # = go build -buildmode=c-shared, GOARM64=v8.0 CGO_CFLAGS=-march=armv8-a  -> 4.5 MB dylib
cp ... plugins/darwin/arm64/management-api-v0.0.1.dylib
./cli-proxy-api -config /tmp/cpa-smoke/config.yaml    # puerto 127.0.0.1:18317
```
Config: `port: 18317`, `remote-management.secret-key: smoke-test-key`, `plugins: {enabled: true, dir: /tmp/cpa-smoke/plugins, configs: {management-api: {enabled: true, priority: 1}}}`.

Logs:
```
pluginhost: plugin loaded plugin_id=management-api version=0.0.1 path=/tmp/cpa-smoke/plugins/darwin/arm64/management-api-v0.0.1.dylib
pluginhost: plugin registered plugin_id=management-api plugin_name=example-management-api-go version=0.1.0 ...
```
- `GET /v0/management/plugins` (Bearer key) → `registered:true, effective_enabled:true, supports_quota:false, supports_oauth:false`, menus `[/v0/resource/plugins/management-api/status]`.
- `GET /v0/resource/plugins/management-api/status` → 200 `text/html` (HTML del plugin). Sin Authorization también dio 200 (el recurso es navegable públicamente: no pongas datos sensibles ahí).
- Capacidades que registró este ejemplo: solo `management_api` (el JSON de metadata del dylib: `"capabilities":{"management_api":true}`).
- **No verificado**: Scheduler / QuotaProvider / ProviderExecutor con un plugin real. `examples/plugin/simple/go` (declara todas) no compiló fuera del repo (`no required module provides package .../v8/sdk/pluginabi`: su go.mod no apunta al clon; haría falta `go.work`/`replace`). Los tipos existen: `sdk/pluginapi/types.go` Capabilities{Scheduler (l.100), SchedulerContinuity, QuotaProvider (l.177)...}; métodos ABI `scheduler.pick`, `scheduler.continuity`, `quota.identifier|describe|fetch|reset`, `management.register|handle`.
- Proceso de prueba detenido (`o2ctl services stop cpa-smoke`; pgrep vacío). Nada de ~/.omp ni de la instancia real tocado.

## 3. Detalles
- Config: `plugins.enabled` (global, default false), `plugins.dir`, `plugins.store-sources`, `plugins.store-auth`, `plugins.configs.<pluginID>.{enabled,priority,version,<claves propias>}`; `enabled` por plugin no cambia el global.
- Descubrimiento: `<dir>/<GOOS>/<GOARCH>/<id>-v<semver>.dylib|.so|.dll`; el id sale del nombre. Se carga una copia "shadow" (`.shadow/`).
- ABI: `sdk/pluginabi.ABIVersion = 1` (uint32). El host lo pasa en `cliproxy_set_host_api`; `loader_unix.go:230` rechaza el plugin si `api.abi_version != 1` ("plugin ABI version %d is not supported"). Además hay `schema_version:1` en metadata.
- Páginas: en este clon NO existe `/plugin-pages/`. Los recursos del plugin (`management.register` → `resources`/`menu`) se sirven bajo `/v0/resource/plugins/<pluginID>/...`; rutas autenticadas del plugin bajo `/v0/management/...`. El Management Center consume `menus` de `GET /v0/management/plugins`. `/plugin-pages` sería concepto de la UI/otra versión: no confirmado.
- Hot reload: sí parcial. Al añadir `management-api-v0.0.2.dylib` y tocar config con el CPA vivo, `/v0/management/plugins` pasó a apuntar al v0.0.2 (selección por versión más alta o `configs.<id>.version`). `ApplyConfig` re-evalúa plugins en cada recarga de config. Pero NUNCA se hace dlclose (comentario en `loader_unix.go`: los plugins Go no son hot-unloadable; la imagen queda mapeada, hay fuga acotada) → recargas repetidas acumulan memoria; reiniciar para actualizaciones limpias.

## 4. Límites
- Plugins son código de confianza en proceso (crash del plugin = crash de CPA).
- cgo obligatorio en host; release `no-plugin` (freebsd) y builds `CGO_ENABLED=0` rechazan plugins (`loader_unsupported.go`).
- Plugin debe ser c-shared del mismo GOOS/GOARCH; fijar baseline (`GOARM64=v8.0 CGO_CFLAGS=-march=armv8-a`) para distribuir. Windows: .dll con otro loader (no probado).
- Pin ABI: compilar contra el mismo `sdk/pluginabi` (v8 module path; el ejemplo aún referencia /v7) y ABIVersion=1; fijar CPA ≥ 8.1.x. Go plugins pesan ~4.5 MB cada uno.
- Recomendación: construir nuestro plugin con el módulo `github.com/router-for-me/CLIProxyAPI/v8` (go.work o `replace` al clon) y repetir smoke con Scheduler/QuotaProvider antes de decidir; luego instalarlo en `cpa-core/plugins/darwin/arm64/` de la instancia real (decisión del usuario).
