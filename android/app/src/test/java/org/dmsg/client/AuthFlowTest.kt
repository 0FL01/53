package org.dmsg.client

import org.junit.Assert.*
import org.junit.Test
import uniffi.dmsg_core.LoginOutcome
import uniffi.dmsg_core.RegistrationPolicy

class AuthFlowTest {
    @Test fun launchRoutesPersistedProfileAndAccount() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        assertEquals(LaunchState.Connection, flow.launchState())
        f.configureDns("dmsg://server/fixture", listOf("127.0.0.1:53"))
        assertEquals(LaunchState.Authentication, AuthFlow(f).launchState())
        f.authenticated = true
        assertEquals(LaunchState.Dialogs, AuthFlow(f).launchState())
    }
    @Test fun policyAlwaysClosesPreAuthCarrier() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        assertEquals(RegistrationPolicy.INVITE_ONLY, flow.policy())
        f.policyFailure = DmsgError("no network", ErrorKind.Transport)
        assertTrue(runCatching { flow.policy() }.isFailure)
        assertEquals(2, f.stopCalls)
    }
    @Test fun invitationOnlySignupAndOnlyWhenPolicyRequiresIt() {
        for (policy in RegistrationPolicy.entries) {
            assertFalse(AuthForm.needsInvitation(AuthAction.Login, policy))
        }
        assertFalse(AuthForm.needsInvitation(AuthAction.Signup, null))
        assertFalse(AuthForm.needsInvitation(AuthAction.Signup, RegistrationPolicy.OPEN))
        assertTrue(AuthForm.needsInvitation(AuthAction.Signup, RegistrationPolicy.INVITE_ONLY))
        val f = FakeFacade(); val flow = AuthFlow(f)
        f.signupProbe = { _, _, invite -> assertNull(invite) }
        flow.submit(AuthAction.Signup, RegistrationPolicy.OPEN, "New.User", AuthSecrets("password".toCharArray(), "ignored".toCharArray()))
        f.signupProbe = { _, _, invite -> assertEquals("A".repeat(43), invite) }
        flow.submit(AuthAction.Signup, RegistrationPolicy.INVITE_ONLY, "another", AuthSecrets("password".toCharArray(), "A".repeat(43).toCharArray()))
        assertEquals(2, f.signupCalls)
    }
    @Test fun boundsUseAsciiLoginAndExactUtf8PasswordWithoutTrimming() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        f.loginProbe = { login, password, _ -> assertEquals("ABC._-012", login); assertEquals(" 123456 ", password) }
        flow.submit(AuthAction.Login, null, "ABC._-012", AuthSecrets(" 123456 ".toCharArray()))
        for (login in listOf("ab", "a".repeat(33), "abc def", "абв", " abc")) {
            assertEquals(ErrorKind.InvalidInput, failure { AuthForm.validate(AuthAction.Login, null, login, "12345678", null) }.kind)
        }
        AuthForm.validate(AuthAction.Login, null, "a".repeat(32), "é".repeat(64), null)
        AuthForm.validate(AuthAction.Login, null, "abc", "é".repeat(4), null)
        for (password in listOf("1234567", "é".repeat(65))) {
            assertEquals(ErrorKind.InvalidInput, failure { AuthForm.validate(AuthAction.Login, null, "abc", password, null) }.kind)
        }
    }
    @Test fun missingMalformedInvitationOrUnknownPolicyCannotSubmitSignup() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        for (invite in listOf("", "A".repeat(42), "A".repeat(42) + "=", "dmsg://server/AAAA")) {
            assertEquals(ErrorKind.InviteRequired, failure {
                flow.submit(AuthAction.Signup, RegistrationPolicy.INVITE_ONLY, "abc", AuthSecrets("password".toCharArray(), invite.toCharArray()))
            }.kind)
        }
        assertTrue(runCatching { flow.submit(AuthAction.Signup, null, "abc", AuthSecrets("password".toCharArray())) }.isFailure)
        assertEquals(0, f.signupCalls)
    }
    @Test fun cancellationWipesPasswordWithoutSecondLoginOrServerChange() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        val password = "password".toCharArray()
        f.outcomes.add(LoginOutcome.ReplacementRequired("a".repeat(64)))
        flow.submit(AuthAction.Login, null, "abc", AuthSecrets(password))
        assertTrue(flow.awaitingConfirmation)
        assertFalse(f.authenticated)
        flow.cancel()
        assertFalse(flow.awaitingConfirmation)
        assertTrue(password.all { it == '\u0000' })
        assertTrue(runCatching { flow.confirm() }.isFailure)
        assertEquals(listOf(Pair("abc", null)), f.loginCalls)
    }
    @Test fun confirmationIsCasLoginAndRepeatedChallengeNeedsAnotherPrompt() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        val password = "password".toCharArray()
        f.outcomes.add(LoginOutcome.ReplacementRequired("a".repeat(64)))
        f.outcomes.add(LoginOutcome.ReplacementRequired("b".repeat(64)))
        f.outcomes.add(LoginOutcome.Authenticated("MOCK1234MOCK"))
        flow.submit(AuthAction.Login, null, "abc", AuthSecrets(password))
        assertEquals(1, f.loginCalls.size)
        assertTrue(flow.confirm() is LoginOutcome.ReplacementRequired)
        assertTrue(flow.awaitingConfirmation)
        assertFalse(f.authenticated)
        assertEquals(2, f.loginCalls.size)
        assertTrue(flow.confirm() is LoginOutcome.Authenticated)
        assertEquals(listOf(null, "a".repeat(64), "b".repeat(64)), f.loginCalls.map { it.second })
        assertFalse(flow.awaitingConfirmation)
        assertTrue(password.all { it == '\u0000' })
        assertEquals(3, f.stopCalls)
    }
    @Test fun typedAuthFailureClearsSecretsAndSocket() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        f.authFailure = DmsgError("wrong password", ErrorKind.InvalidCredentials)
        val password = "password".toCharArray()
        assertEquals(ErrorKind.InvalidCredentials, failure {
            flow.submit(AuthAction.Login, null, "abc", AuthSecrets(password))
        }.kind)
        assertTrue(password.all { it == '\u0000' })
        assertFalse(flow.awaitingConfirmation)
        assertEquals(1, f.stopCalls)
    }
    @Test fun lifecycleCancellationDuringLoginCannotResurrectConfirmation() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        val password = "password".toCharArray()
        f.outcomes.add(LoginOutcome.ReplacementRequired("a".repeat(64)))
        f.loginProbe = { _, _, _ ->
            flow.cancel()
            assertTrue(password.all { it == '\u0000' })
        }
        flow.submit(AuthAction.Login, null, "abc", AuthSecrets(password))
        assertFalse(flow.awaitingConfirmation)
        assertFalse(f.authenticated)
        assertEquals(1, f.loginCalls.size)
    }
    @Test fun anotherSubmissionInvalidatesPreviousConfirmation() {
        val f = FakeFacade(); val flow = AuthFlow(f)
        val previous = "old-password".toCharArray()
        f.outcomes.add(LoginOutcome.ReplacementRequired("a".repeat(64)))
        flow.submit(AuthAction.Login, null, "abc", AuthSecrets(previous))
        f.authFailure = DmsgError("wrong password", ErrorKind.InvalidCredentials)
        assertTrue(runCatching { flow.submit(AuthAction.Login, null, "abc", AuthSecrets("new-password".toCharArray())) }.isFailure)
        assertTrue(previous.all { it == '\u0000' })
        assertFalse(flow.awaitingConfirmation)
    }
    private fun failure(block: () -> Unit): DmsgError {
        try { block(); fail("must fail") } catch (e: DmsgError) { return e }
        throw AssertionError()
    }
}
