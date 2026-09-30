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
    is FfiException.InvalidInput -> DmsgError("Проверьте логин, пароль и приглашение", ErrorKind.InvalidInput)
    is FfiException.BadArgs -> DmsgError("Неверные параметры")
    is FfiException.BadQr -> DmsgError("Неверный или повреждённый код")
    is FfiException.PinMismatch -> DmsgError("Сертификат сервера не совпадает с кодом подключения", ErrorKind.PinMismatch)
    is FfiException.NotEnrolled -> DmsgError("Нужно войти в аккаунт", ErrorKind.NotAuthenticated)
    is FfiException.UnknownContact -> DmsgError("Контакт не найден")
    is FfiException.NotAccepted -> DmsgError("Запрос контакта ещё не принят")
    is FfiException.Blocked -> DmsgError("Контакт заблокирован")
    is FfiException.IdentityMismatch -> DmsgError("Ключ контакта изменился. Отправка СТОП до подтверждения", ErrorKind.IdentityMismatch)
    is FfiException.NothingToConfirm -> DmsgError("Нет нового ключа для подтверждения")
    is FfiException.MissingKeys -> DmsgError("Нет ключей контакта. Сканируйте его QR")
    is FfiException.NoPeerPrekeys -> DmsgError("У контакта закончились одноразовые ключи")
    is FfiException.UploadRejected -> DmsgError("Сервер отклонил загрузку ключей")
    is FfiException.Quota -> DmsgError("Превышена квота сервера")
    is FfiException.Revoked -> DmsgError("Доступ этого устройства отозван")
    is FfiException.Busy -> DmsgError("Сервер занят. Попробуйте позже")
    is FfiException.BadText -> DmsgError("Сообщение пустое или слишком длинное")
    is FfiException.Transport -> DmsgError("Нет связи с сервером. Проверьте сеть и DNS", ErrorKind.Transport)
    is FfiException.Store -> DmsgError("Ошибка локального хранилища")
    is FfiException.Crypto -> DmsgError("Ошибка проверки криптографических данных")
    is FfiException.Protocol -> DmsgError("Несовместимый или повреждённый протокол")
    is FfiException.Server -> DmsgError("Сервер отклонил запрос")
}

internal fun ffiErrorMessage(error: FfiException): String = ffiError(error).message.orEmpty()
internal fun humanError(error: Throwable): String =
    if (error is DmsgError) error.message.orEmpty() else "Не удалось выполнить операцию"
