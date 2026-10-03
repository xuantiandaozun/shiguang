// Exercise frontend migrations and model switching against the actual TS modules.
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const ts = require("typescript");
const cache = new Map();
function load(file) {
  file = path.resolve(file);
  if (cache.has(file)) return cache.get(file).exports;
  const module = { exports: {} };
  cache.set(file, module);
  const source = ts.transpileModule(fs.readFileSync(file, "utf8"), { compilerOptions: { module:ts.ModuleKind.CommonJS, target:ts.ScriptTarget.ES2022 } }).outputText;
  new Function("require", "module", "exports", source)((id) => id.startsWith(".") ? load(path.resolve(path.dirname(file), `${id}.ts`)) : require(id), module, module.exports);
  return module.exports;
}
const models = load("src/lib/models.ts");
const profile = (id, model) => ({ id, name:id, base_url:`https://${id}.example/v1`, api_key:"test-key", model, thinking_enabled:false, reasoning_effort:"low", models:[{ model, label:model, supports_vision:false, context_window:128000 }] });
const a = profile("a", "provider-a-exact-name");
const b = profile("b", "provider-b-exact-name");
assert.equal(models.syncProfileModel(a).model, "provider-a-exact-name");
assert.deepEqual(models.migrateSettingsProfiles([a,b]).map((p) => p.models.map((m) => m.model)), [["provider-a-exact-name"],["provider-b-exact-name"]]);
assert.equal(models.llmProfileStatus({ ...a, valid_until:"2000-01-01T00:00" }), "expired");
assert.equal(models.llmProfileStatus({ ...a, valid_until:"2099-01-01T00:00" }), "ready");
const roles = [{ role:"browser", enabled:true, profile_id:"", model:a.model }, { role:"files", enabled:true, profile_id:"a", model:a.model }];
const switched = models.reconcileSubagentRoles([a,b], "b", roles);
assert.equal(switched[0].model, "");
assert.equal(switched[1].model, a.model);
const deleted = models.reconcileSubagentRoles([b], "b", roles);
assert.equal(deleted[1].profile_id, "");
assert.equal(deleted[1].model, "");
const legacy = models.migrateProfile({ ...a, model:"manual-old-model", models:undefined });
assert.equal(legacy.models[0].model, "manual-old-model");
assert.equal(legacy.model, "manual-old-model");
console.log("Frontend profile migration, expiry and provider isolation checks passed.");
