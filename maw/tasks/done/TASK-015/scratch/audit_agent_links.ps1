$ErrorActionPreference = 'Stop'
$project = Join-Path $env:USERPROFILE '.claude/projects/C--Users-user-dev-cctg'
$files = @(Get-ChildItem -LiteralPath $project -File -Filter '*.jsonl')
$calls = @{}
$results = @{}
$bad = 0
foreach ($file in $files) {
    $lineNo = 0
    foreach ($line in [IO.File]::ReadLines($file.FullName)) {
        $lineNo++
        try { $record = $line | ConvertFrom-Json } catch { $bad++; continue }
        if ($record.type -eq 'assistant' -and $record.message.content -is [Array]) {
            foreach ($block in $record.message.content) {
                if ($block.type -eq 'tool_use' -and $block.name -eq 'Agent' -and $block.id) {
                    $calls[[string]$block.id] = $lineNo
                }
            }
        }
        if ($record.type -eq 'user' -and $record.message.content -is [Array]) {
            foreach ($block in $record.message.content) {
                if ($block.type -eq 'tool_result' -and $block.tool_use_id) {
                    $agentId = $null
                    if ($record.toolUseResult -and $record.toolUseResult -isnot [string]) {
                        $agentId = $record.toolUseResult.agentId
                    }
                    $results[[string]$block.tool_use_id] = -not [string]::IsNullOrWhiteSpace([string]$agentId)
                }
            }
        }
    }
}
$withResult = 0
$withAgentId = 0
foreach ($id in $calls.Keys) {
    if ($results.ContainsKey($id)) {
        $withResult++
        if ($results[$id]) { $withAgentId++ }
    }
}
"parent_jsonl_files=$($files.Count)"
"invalid_json_lines=$bad"
"agent_calls=$($calls.Count)"
"calls_with_result=$withResult"
"calls_with_agent_id=$withAgentId"
"calls_without_result=$($calls.Count - $withResult)"
"results_without_agent_id=$($withResult - $withAgentId)"
