# cctg: Claude Code в Telegram

[![ci](https://github.com/pockerhead/cctg/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/pockerhead/cctg/actions/workflows/ci.yml)
[![release](https://img.shields.io/github/v/release/pockerhead/cctg)](https://github.com/pockerhead/cctg/releases/latest)
[![image](https://img.shields.io/badge/ghcr.io-pockerhead%2Fcctg-blue?logo=docker)](https://github.com/pockerhead/cctg/pkgs/container/cctg)

Сессии Claude Code на ваших машинах видны в одной закрытой Telegram-группе с темами. Каждая папка проекта получает свою тему: там видно, что делает Claude, приходят его ответы и кнопки разрешений, а ваше сообщение в теме уходит в сессию.

```
ноутбук, ПК, сервер          сервер с Docker             Telegram
claude-cctg в папке  ── TLS ──>  cctg hub  ── Bot API ──>  группа: тема на папку
```

Hub это один процесс с ботом. Он живёт на сервере (или на одной из ваших машин). На каждой машине с Claude Code стоит клиент `cctg`, его ставит одна команда.

## Что нужно

- Telegram-бот и группа с темами (ниже, 5 минут).
- Сервер с Docker для hub. Можно и без сервера: hub на своей машине, `docs/poc.md`.
- На каждой машине с сессиями: Linux, macOS (Apple Silicon) или Windows. Claude Code установщик поставит сам, если его нет.
- Windows: Git for Windows (в нём Git Bash, через него работают установщик и хуки cctg). Если его нет: `winget install Git.Git`, потом откройте Git Bash.

## 1. Бот и группа

1. В Telegram у @BotFather: `/newbot`, получить токен бота.
2. Создать группу, в настройках включить «Темы» (Topics).
3. Добавить бота в группу и сделать админом с правами «Управление темами», «Удаление сообщений», «Закрепление сообщений».
4. Написать в группу любое сообщение и открыть `https://api.telegram.org/bot<токен>/getUpdates`: `chat.id` (начинается с `-100`) это id группы, `from.id` это ваш id.

## 2. Hub на сервере

На сервере с Docker:

```sh
curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/v0.1.0/install.sh | sh -s -- --hub
```

Установщик спросит токен бота (не показывается при вводе), id группы, ваш id, адрес сервера и, если Telegram с сервера доступен только через прокси, адрес прокси. Он сам сделает секрет и сертификат, запустит hub в Docker и проверит бота и группу. В конце он печатает строку для установки клиентов. В ней секрет hub: вставляйте её только на своих машинах.

Подробности (прокси, обновление, порты): `docs/remote-hub.md`.

## 3. Клиент на каждой машине

Выполните строку, которую напечатал hub (в Windows это Git Bash). Она выглядит так:

```sh
curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/v0.1.0/install.sh | CCTG_HUB_SECRET='...' sh -s -- --hub-host <сервер> --pin <отпечаток>
```

Без строки можно так, установщик всё спросит (секрет при вводе не показывается):

```sh
curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/v0.1.0/install.sh | sh
```

Установщик ставит `cctg` в `~/.cctg`, команду `claude-cctg` в `~/.local/bin` и проверяет связь с hub. Ваши `~/.claude/settings.json` и `~/.claude.json` он не трогает.

Проверить связь потом: `~/.cctg/bin/cctg doctor`.

## 4. Запуск

В папке проекта вместо `claude`:

```sh
claude-cctg
```

Все аргументы `claude` работают так же (`claude-cctg --resume`, `claude-cctg "почини тест"`). Claude Code при старте спрашивает про development channels: выберите пункт 1. В Windows в обычной консоли (PowerShell, cmd, Windows Terminal), на Linux и macOS в терминале cctg нажимает за вас; в окне Git Bash (mintty) нет, поэтому сессии в Windows лучше запускать из PowerShell или cmd.

## Что видно в Telegram

- Тема `[машина] папка · название сессии`. Иконка показывает, жива сессия, ждёт ли разрешения.
- Ваши промпты, текст Claude, по строке на каждый вызов инструмента и финальный ответ хода.
- Запрос разрешения приходит с кнопками Allow и Deny.
- Закреплённое сообщение со статусом: модель, контекст, лимиты.
- Сообщение в теме уходит в сессию. Файлы и фото тоже.
- `/brief` и `/full` в теме присылают транскрипт сессии.
- Сессия закрыта: сообщения ждут в теме (до 50) до следующего запуска в этой папке.

## Обновление

Команды выше ставят релиз `v0.1.0`. Новый релиз это та же команда с его тегом (актуальная строка в этом README на `main`).

- Клиент: после обновления hub работающие сессии получают версию hub по кнопке «⬆️ Обновить» в теме: клиент сам скачивает сборку того же релиза из GitHub Releases, сверяет `SHA256SUMS` и ставит её на место старой. Конфиги и обёртку обновляет команда установки нового тега.
- Hub: команда `--hub` нового тега (hub стоит на образе своего релиза, поэтому одного `docker compose pull` мало).

## Удаление

```sh
curl -fsSL https://raw.githubusercontent.com/pockerhead/cctg/v0.1.0/install.sh | sh -s -- --uninstall
```

Удаляет только то, что поставил установщик. Hub: та же строка с `--hub --uninstall` вместо `--uninstall` (останавливает контейнер, `hub.env`, ключ и состояние остаются).
