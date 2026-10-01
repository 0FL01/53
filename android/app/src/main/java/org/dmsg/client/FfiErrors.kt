package org.dmsg.client

import uniffi.dmsg_core.FfiException

/** Typed errors, static human messages: never reflect native payloads or credentials. */
internal fun ffiError(error: FfiException): DmsgError = when (error) {
    is FfiException.InvalidCredentials -> DmsgError("Неверный логин или пароль", ErrorKind.InvalidCredentials)
    is FfiException.LoginTaken -> DmsgError("Логин уже занят", ErrorKind.LoginTaken)
    is FfiException.InviteRequired -> DmsgError("Для регистрации нужно приглашение", ErrorKind.InviteRequired)
    is FfiException.InviteExpired -> DmsgError("Срок приглашения истёк", ErrorKind.InviteExpired)
    is FfiException.InviteRevoked -> DmsgError("Приглашение отозвано", ErrorKind.InviteRevoked)
    is FfiException.InviteUsed -> DmsgError("Приглашение уже использовано", ErrorKind.InviteUsed)
    is FfiException.AuthRateLimited -> DmsgError("Слишком много попыток. Попробуйте позже", ErrorKind.AuthRateLimited)
    is FfiException.InvalidInput -> DmsgError("Проверьте введённые данные", ErrorKind.InvalidInput)
    is FfiException.BadArgs -> DmsgError("Неверные параметры", ErrorKind.InvalidInput)
    is FfiException.BadQr -> DmsgError("Неверный или повреждённый код", ErrorKind.BadQr)
    is FfiException.PinMismatch -> DmsgError("Сертификат сервера не совпадает с кодом подключения", ErrorKind.PinMismatch)
    is FfiException.NotEnrolled -> DmsgError("Нужно войти в аккаунт", ErrorKind.NotAuthenticated)
    is FfiException.UnknownContact -> DmsgError("Контакт не найден", ErrorKind.UnknownContact)
    is FfiException.NotAccepted -> DmsgError("Запрос контакта ещё не принят", ErrorKind.NotAccepted)
    is FfiException.Blocked -> DmsgError("Контакт заблокирован", ErrorKind.Blocked)
    is FfiException.IdentityMismatch -> DmsgError("Ключ контакта изменился. Отправка СТОП до подтверждения", ErrorKind.IdentityMismatch)
    is FfiException.NothingToConfirm -> DmsgError("Нет нового ключа для подтверждения")
    is FfiException.MissingKeys -> DmsgError("Нет ключей контакта. Сканируйте его QR", ErrorKind.MissingKeys)
    is FfiException.NoPeerPrekeys -> DmsgError("У контакта закончились одноразовые ключи")
    is FfiException.UploadRejected -> DmsgError("Сервер отклонил загрузку ключей")
    is FfiException.Quota -> DmsgError("Превышена квота сервера")
    is FfiException.Revoked -> DmsgError("Доступ этого устройства отозван", ErrorKind.Revoked)
    is FfiException.Busy -> DmsgError("Сервер занят. Попробуйте позже", ErrorKind.Busy)
    is FfiException.BadText -> DmsgError("Сообщение пустое или длиннее 4096 байт UTF-8", ErrorKind.BadText)
    is FfiException.Transport -> DmsgError("Нет связи с сервером. Проверьте сеть и DNS", ErrorKind.Transport)
    is FfiException.Store -> DmsgError("Ошибка локального хранилища. Данные не сброшены", ErrorKind.Store)
    is FfiException.Crypto -> DmsgError("Ошибка проверки криптографических данных", ErrorKind.Crypto)
    is FfiException.Protocol -> DmsgError("Несовместимый или повреждённый протокол", ErrorKind.Protocol)
    is FfiException.Server -> DmsgError("Сервер отклонил запрос")
}

internal fun ffiErrorMessage(error: FfiException): String = ffiError(error).message.orEmpty()
internal fun humanError(error: Throwable): String {
    if (error !is DmsgError) return "Не удалось выполнить операцию"
    return when (error.kind) {
        ErrorKind.StorageKeyLost -> "Исходный Keystore-ключ недоступен. Историю и локальную копию восстановить в этой установке нельзя; новая identity не создана."
        ErrorKind.SnapshotMissing -> "Защищённой локальной копии пока нет"
        ErrorKind.LiveDatabaseExists -> "Рабочая база уже существует. Восстановление не перезаписывает identity и историю."
        ErrorKind.LiveDatabaseMissing -> "Рабочей базы пока нет. Откройте аккаунт, затем создайте копию."
        ErrorKind.SnapshotRestoreRequired -> "Сначала восстановите существующую локальную копию на экране «Хранилище»"
        ErrorKind.SnapshotInvalid -> "Локальная копия или исходный Keystore-ключ не прошли проверку. Identity и рабочая база не заменены."
        ErrorKind.Store -> "Ошибка защищённого хранилища. Данные не сброшены; существующая копия сохранена."
        else -> error.message.orEmpty()
    }
}
