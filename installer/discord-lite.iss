; Instalador de discord-lite (Inno Setup 6).
; Compilar:  ISCC.exe installer\discord-lite.iss   (desde la raíz del proyecto)
; Salida:    dist\discord-lite-setup-<versión>.exe
;
; Instala el exe + icono en Archivos de programa (per-user, sin admin), crea
; accesos directos en el menú Inicio y el Escritorio, registra el desinstalador
; y al terminar ofrece abrir la configuración de privacidad del micrófono de
; Windows: si el micrófono está bloqueado para apps de escritorio, la app
; captura silencio y "no te oyen" aunque todo lo demás funcione.

#define MyAppName "discord-lite"
#define MyAppVersion "0.3.0"
#define MyAppPublisher "BetoCW"
#define MyAppURL "https://github.com/BetoCW/Discord-BuildInRust"
#define MyAppExeName "discord-lite.exe"

[Setup]
; GUID fijo del producto: las actualizaciones se instalan encima.
AppId={{7E1C3B9A-4D2F-4A57-9C66-DB10C5A7E3F1}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
; Per-user: no pide administrador (instala en %LOCALAPPDATA%\Programs).
PrivilegesRequired=lowest
OutputDir=..\dist
OutputBaseFilename=discord-lite-setup-{#MyAppVersion}
SetupIconFile=..\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern

[Languages]
Name: "spanish"; MessagesFile: "compiler:Languages\Spanish.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\icon.ico"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent
; Muy importante para la voz: si Windows tiene bloqueado el micrófono para
; aplicaciones de escritorio, NADIE te oirá. Esta casilla abre esa página.
Filename: "ms-settings:privacy-microphone"; Description: "Revisar el permiso del micrófono de Windows (recomendado)"; Flags: postinstall shellexec skipifsilent unchecked

[UninstallDelete]
; Log que la app escribe junto al exe.
Type: files; Name: "{app}\discord-lite.log"

[Messages]
spanish.FinishedLabelNoIcons=La instalación de [name] se completó.%n%nSi tus amigos no te oyen en voz: abre Inicio → Configuración → Privacidad y seguridad → Micrófono y activa «Permitir que las aplicaciones de escritorio accedan al micrófono». Después, en discord-lite usa «⚙ Ajustes de voz» para elegir tu micrófono y probarlo.
spanish.FinishedLabel=La instalación de [name] se completó.%n%nSi tus amigos no te oyen en voz: abre Inicio → Configuración → Privacidad y seguridad → Micrófono y activa «Permitir que las aplicaciones de escritorio accedan al micrófono». Después, en discord-lite usa «⚙ Ajustes de voz» para elegir tu micrófono y probarlo.
