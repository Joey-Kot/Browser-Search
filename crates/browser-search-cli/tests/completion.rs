use std::{env, io::ErrorKind, path::Path, process::Command};

#[test]
fn powershell_legacy_completes_a_quoted_empty_shell_name() {
    let binary = env!("CARGO_BIN_EXE_search");
    let mut paths = vec![Path::new(binary).parent().unwrap().to_owned()];
    paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));

    let output = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            r#"
            $ErrorActionPreference = 'Stop'
            & $env:SEARCH_TEST_BIN completion powershell | Out-String | Invoke-Expression
            $PSNativeCommandArgumentPassing = 'Legacy'
            $line = "search completion ''"
            $result = [System.Management.Automation.CommandCompletion]::CompleteInput(
                $line, $line.Length, $null
            )
            if ($result.CompletionMatches.CompletionText -notcontains "'fish'") {
                throw 'Expected a quoted shell name'
            }
            "#,
        ])
        .env("SEARCH_TEST_BIN", binary)
        .env("PATH", env::join_paths(paths).unwrap())
        .env_remove("SEARCH_COMPLETE")
        .env_remove("SEARCH_API_KEY")
        .output();

    match output {
        Ok(output) => assert!(output.status.success(), "{output:?}"),
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => panic!("could not run PowerShell: {error}"),
    }
}
