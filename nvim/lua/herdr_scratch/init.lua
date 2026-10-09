-- Consume the context published by herdr-scratch on each popup open. This
-- module owns the watcher; consumers such as Sidekick only need the User event.
local M = {}
local started = false

local function context_path()
  local path = vim.env.HERDR_SCRATCH_CONTEXT
  if path and path ~= "" then
    return path
  end
  -- A shell that predates the variable still has TMUX, so query the updated
  -- session environment instead of requiring that shell to be restarted.
  local output = vim.fn.system({ "tmux", "show-environment", "HERDR_SCRATCH_CONTEXT" })
  if vim.v.shell_error == 0 then
    return output:match("^HERDR_SCRATCH_CONTEXT=([^\r\n]+)")
  end
end

local function read_context(path)
  local file = vim.uv.fs_open(path, "r", 0)
  if not file then
    return
  end
  -- Read and identify the same inode, even if another publication replaces the
  -- pathname while we read. Identical JSON written again is still a new event.
  local stat = vim.uv.fs_fstat(file)
  local document = stat and vim.uv.fs_read(file, stat.size, 0)
  vim.uv.fs_close(file)
  local ok, context = pcall(vim.json.decode, document)
  if ok and type(context) == "table" and context.version == 1
      and type(context.root) == "string" and context.root ~= ""
      and type(context.source_pane) == "string" and context.source_pane ~= "" then
    local publication = table.concat({ stat.ino, stat.mtime.sec, stat.mtime.nsec }, ":")
    return context, publication
  end
end

local function start()
  local path = context_path()
  if not path then
    return
  end
  path = vim.fn.fnamemodify(path, ":p")
  local directory, filename = vim.fs.dirname(path), vim.fs.basename(path)
  local watcher, timer = vim.uv.new_fs_event(), vim.uv.new_timer()
  if not watcher or not timer then
    if watcher then
      watcher:close()
    end
    if timer then
      timer:close()
    end
    return
  end

  local stopped = false
  local last_root, last_publication

  local function refresh(initial)
    if stopped then
      return
    end
    local context, publication = read_context(path)
    if not context or publication == last_publication then
      return
    end
    last_publication = publication
    if initial then
      -- new-session already supplied the startup cwd. Record the publication
      -- without overriding a shell's cwd or a user's startup configuration.
      last_root = context.root
      return
    end
    local changed = false
    if context.root ~= last_root and vim.fn.isdirectory(context.root) == 1 then
      changed = pcall(vim.api.nvim_set_current_dir, context.root)
      if changed then
        last_root = context.root
        local repo, branch = context.root:match("([^/]+)/%.wt/(.+)$")
        local label = repo and (repo .. "/" .. branch) or vim.fs.basename(context.root)
        vim.notify("scratch → " .. label, vim.log.levels.INFO)
      end
    end
    -- Every publication refreshes parent-affinity consumers, even if a manual
    -- :cd is being preserved because the published root did not change.
    vim.api.nvim_exec_autocmds("User", {
      pattern = "HerdrScratchContext",
      data = { root = context.root, source_pane = context.source_pane, changed = changed },
    })
  end

  local function stop()
    stopped = true
    watcher:stop()
    timer:stop()
    watcher:close()
    timer:close()
  end

  -- Atomic publication replaces the file's inode. Watching the directory
  -- survives renames; debounce and inode/mtime identity suppress duplicate OS
  -- events. Editor API calls must run on Neovim's main loop, not a uv callback.
  local ok = watcher:start(directory, {}, function(err, name)
    if stopped or err or (name and vim.fs.basename(name) ~= filename) then
      return
    end
    timer:stop()
    timer:start(40, 0, vim.schedule_wrap(function()
      refresh(false)
    end))
  end)
  if not ok then
    stop()
    return
  end
  refresh(true)
  vim.api.nvim_create_autocmd("VimLeavePre", { once = true, callback = stop })
end

function M.setup()
  if started or not vim.env.HERDR_SCRATCH_SOURCE_PANE then
    return
  end
  started = true
  if vim.v.vim_did_enter == 1 then
    start()
  else
    vim.api.nvim_create_autocmd("VimEnter", { once = true, callback = start })
  end
end

return M
