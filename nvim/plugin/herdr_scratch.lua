-- Only scratch editors participate in the parent-pane context handoff.
if vim.g.loaded_herdr_scratch or not vim.env.HERDR_SCRATCH_SOURCE_PANE then
  return
end
vim.g.loaded_herdr_scratch = true
require("herdr_scratch").setup()
