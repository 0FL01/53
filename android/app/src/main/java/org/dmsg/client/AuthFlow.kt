package org.dmsg.client

import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.RegistrationPolicy

internal enum class LaunchState { Connection, Authentication, Dialogs }
internal enum class AuthAction { Login, Signup }

/** No password normalization: bounds are bytes, not Kotlin UTF-16 characters. */
internal object AuthForm {
    fun needsInvitation(action: AuthAction, policy: RegistrationPolicy?) =
        action == AuthAction.Signup && policy == RegistrationPolicy.INVITE_ONLY

    fun validate(action: AuthAction, policy: RegistrationPolicy?, login: String, password: String, invitation: String?) {
        // Core performs canonical ASCII lowercasing. Pass the original login to its builder.
        if (login.length !in 3..32)
            throw DmsgError("Логин должен содержать от 3 до 32 знаков", ErrorKind.InvalidInput)
        if (login.any {
                it !in 'a'..'z' && it !in 'A'..'Z' && it !in '0'..'9' && it != '_' && it != '-' && it != '.'
            }) throw DmsgError("Проверьте логин: используйте латинские буквы и цифры, без пробелов", ErrorKind.InvalidInput)
        val passwordBytes = password.toByteArray(Charsets.UTF_8).size
        if (passwordBytes < 8)
            throw DmsgError("Пароль слишком короткий. Добавьте ещё несколько знаков", ErrorKind.InvalidInput)
        if (passwordBytes > 128)
            throw DmsgError("Пароль слишком длинный. Сделайте его короче", ErrorKind.InvalidInput)
        if (action == AuthAction.Signup && policy == null) throw DmsgError("Сначала проверьте режим регистрации сервера")
        if (needsInvitation(action, policy) && (invitation == null || !InvitationInput.isCanonical(invitation)))
            throw DmsgError("Сканируйте приглашение или выберите его приватный файл", ErrorKind.InviteRequired)
    }
}

/** Transient mutable buffers only; never a Bundle, preference, URI or log value. */
internal class AuthSecrets(password: CharArray, invitation: CharArray = charArrayOf()) {
    private val password = password
    private val invitation = invitation
    @Synchronized fun password(): String = password.concatToString()
    @Synchronized fun invitation(): String = invitation.concatToString()
    @Synchronized fun clear() { password.fill('\u0000'); invitation.fill('\u0000') }
}

/** Blocking operations are dispatched by the shell. Cancellation never invokes login. */
internal class AuthFlow(private val f: DmsgFacade) {
    private data class Pending(val login: String, val secrets: AuthSecrets, val expectedDevice: String)
    private val lock = Any()
    private var pending: Pending? = null
    private var active: AuthSecrets? = null
    private var generation = 0L
    val awaitingConfirmation: Boolean get() = synchronized(lock) { pending != null }

    fun launchState(): LaunchState = when {
        f.account().authenticated -> LaunchState.Dialogs
        f.dnsProfile() != null -> LaunchState.Authentication
        else -> LaunchState.Connection
    }

    fun policy(): RegistrationPolicy = try { f.registrationPolicyDns() } finally { f.dnsStop() }

    fun cancel() = synchronized(lock) {
        generation++
        pending?.secrets?.clear()
        pending = null
        active?.clear()
        active = null
    }

    fun submit(action: AuthAction, policy: RegistrationPolicy?, login: String, secrets: AuthSecrets): LoginOutcome {
        val stamp = synchronized(lock) {
            generation++
            pending?.secrets?.clear()
            pending = null
            active?.clear()
            active = secrets
            generation
        }
        var retained = false
        try {
            val password = secrets.password()
            val invitation = if (AuthForm.needsInvitation(action, policy)) secrets.invitation() else null
            AuthForm.validate(action, policy, login, password, invitation)
            val outcome = if (action == AuthAction.Signup) {
                val account = f.signupDns(login, password, invitation)
                if (!account.authenticated || account.contactId == null) throw DmsgError("Сервер не подтвердил аккаунт")
                LoginOutcome.Authenticated(account.contactId!!)
            } else f.loginDns(login, password, null)
            if (outcome is LoginOutcome.ReplacementRequired) synchronized(lock) {
                if (stamp == generation) {
                    checkDevice(outcome.expectedDevice)
                    pending?.secrets?.clear()
                    pending = Pending(login, secrets, outcome.expectedDevice)
                    retained = true
                }
            }
            return outcome
        } finally {
            synchronized(lock) { if (active === secrets) active = null }
            if (!retained) secrets.clear()
            f.dnsStop() // No idle pre-auth socket while typing or deciding.
        }
    }

    fun confirm(): LoginOutcome {
        val stamp: Long
        val attempt: Pending
        synchronized(lock) {
            attempt = pending ?: throw DmsgError("Подтверждение отменено. Введите пароль заново")
            pending = null
            active = attempt.secrets
            stamp = generation
        }
        var retained = false
        try {
            val outcome = f.loginDns(attempt.login, attempt.secrets.password(), attempt.expectedDevice)
            // CAS changed: require a new explicit prompt for the new old-device key.
            if (outcome is LoginOutcome.ReplacementRequired) synchronized(lock) {
                if (stamp == generation) {
                    checkDevice(outcome.expectedDevice)
                    pending = attempt.copy(expectedDevice = outcome.expectedDevice)
                    retained = true
                }
            }
            return outcome
        } finally {
            synchronized(lock) { if (active === attempt.secrets) active = null }
            if (!retained) attempt.secrets.clear()
            f.dnsStop()
        }
    }

    private fun checkDevice(value: String) {
        if (value.length != 64 || value.any { it !in '0'..'9' && it !in 'a'..'f' })
            throw DmsgError("Некорректный ответ подтверждения устройства")
    }
}
