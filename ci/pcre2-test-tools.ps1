param(
    [Parameter(Mandatory = $true, Position = 0)]
    [ValidateSet("printf", "trnull")]
    [string]$Mode,

    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Values
)

$output = [Console]::OpenStandardOutput()
if ($Mode -eq "printf") {
    if ($Values.Count -lt 1) {
        throw "printf requires a format string"
    }
    $text = $Values[0].Replace("\r", "`r").Replace("\n", "`n").Replace("\0", "`0")
    if ($Values.Count -ge 2) {
        $text = $text.Replace("%s", $Values[1])
    }
    $bytes = [Text.Encoding]::UTF8.GetBytes($text)
    $output.Write($bytes, 0, $bytes.Length)
    exit 0
}

$inputStream = [Console]::OpenStandardInput()
$buffer = [byte[]]::new(8192)
while (($count = $inputStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
    for ($index = 0; $index -lt $count; $index++) {
        if ($buffer[$index] -eq 0) {
            $buffer[$index] = [byte][char]'@'
        }
    }
    $output.Write($buffer, 0, $count)
}
