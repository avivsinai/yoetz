import { chatgptSiteAdapter } from "./chatgpt.js";

const adapters = new Map([
  [chatgptSiteAdapter.recipe, chatgptSiteAdapter]
]);

export function advertisedRecipes() {
  return [...adapters.keys()];
}

export function siteAdapterForRecipe(value) {
  const recipe = value == null ? "chatgpt" : value;
  const adapter = typeof recipe === "string" ? adapters.get(recipe) : null;
  if (adapter) {
    return adapter;
  }
  const error = new Error(`recipe ${JSON.stringify(recipe)} is not available in this extension build; rejected before side effects`);
  error.code = "unsupported_recipe";
  throw error;
}
