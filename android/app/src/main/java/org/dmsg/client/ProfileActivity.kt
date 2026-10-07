package org.dmsg.client

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Intent
import android.graphics.Bitmap
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.ImageView
import android.widget.TextView
import androidx.appcompat.app.AlertDialog
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter

/** Own public QR and contact card are separate modes. No private-key/fingerprint invention. */
class ProfileActivity : DmsgActivity() {
    private lateinit var peerId: EditText
    private lateinit var alias: EditText
    private lateinit var info: TextView
    private var ownId: String? = null
    private var contact: Dialog? = null
    private val guard = UiGuard()
    private var active = false
    private var prompt: AlertDialog? = null
    private var aliasDirty = false
    private val mine get() = intent.getBooleanExtra("mine", false)

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_profile)
        NativeUi.back(this, getString(if (mine) R.string.title_my_qr else R.string.title_contact))
        peerId = findViewById(R.id.peer_id)
        alias = findViewById(R.id.contact_alias)
        info = findViewById(R.id.info)
        peerId.setText(intent.getStringExtra("peer").orEmpty())
        findViewById<View>(R.id.mine_panel).visibility = if (mine) View.VISIBLE else View.GONE
        findViewById<View>(R.id.contact_panel).visibility = if (mine) View.GONE else View.VISIBLE
        findViewById<Button>(R.id.btn_request).setOnClickListener {
            val id = peer()
            act(getString(R.string.adding_contact), { request(id) }) { loadContact() }
        }
        findViewById<Button>(R.id.btn_accept).setOnClickListener {
            val id = contact?.contactId ?: return@setOnClickListener
            if (contactCta(contact) == ContactCta.Accept) act(getString(R.string.accepting_contact), {
                accept(id)
                // Consent is durable and local. A temporary network failure
                // must not trap an accepted contact behind the approval UI.
                try { DmsgService.check(this) } catch (e: DmsgError) {
                    if (e.kind != ErrorKind.Transport && e.kind != ErrorKind.Busy) throw e
                }
            }) {
                startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", id))
                finish()
            }
        }
        findViewById<Button>(R.id.btn_block).setOnClickListener {
            if (guard.pending || contact == null || contact?.state == "blocked") return@setOnClickListener
            val id = requireNotNull(contact).contactId
            prompt = AlertDialog.Builder(this).setTitle(R.string.block_title)
                .setMessage(R.string.block_warning)
                .setNegativeButton(R.string.cancel, null).setPositiveButton(R.string.block_confirm) { _, _ ->
                    act(getString(R.string.blocking_contact), { block(id) }) { loadContact() }
                }.show()
        }
        findViewById<Button>(R.id.btn_confirm).setOnClickListener {
            if (guard.pending || contactCta(contact) != ContactCta.VerifyChanged) return@setOnClickListener
            val id = requireNotNull(contact).contactId
            prompt = AlertDialog.Builder(this).setTitle(R.string.confirm_key_title)
                .setMessage(R.string.confirm_key_warning)
                .setNegativeButton(R.string.cancel, null).setPositiveButton(R.string.confirm_key) { _, _ ->
                    act(getString(R.string.confirming_key), { confirm(id) }) { loadContact() }
                }.show()
        }
        findViewById<Button>(R.id.btn_check).setOnClickListener { loadContact() }
        findViewById<Button>(R.id.btn_contact_scan).setOnClickListener {
            startActivity(Intent(this, ScannerActivity::class.java))
        }
        findViewById<Button>(R.id.btn_alias_save).setOnClickListener {
            val value = alias.text.toString().takeIf { it.isNotBlank() }
            val id = contact?.contactId ?: return@setOnClickListener
            act(getString(R.string.saving_alias), { setContactAlias(id, value) }) { aliasDirty = false; loadContact() }
        }
        findViewById<Button>(R.id.btn_alias_clear).setOnClickListener {
            val id = contact?.contactId ?: return@setOnClickListener
            act(getString(R.string.clearing_alias), { setContactAlias(id, null) }) { alias.setText(""); aliasDirty = false; loadContact() }
        }
        findViewById<Button>(R.id.btn_contact_chat).setOnClickListener {
            val id = contact?.contactId ?: return@setOnClickListener
            startActivity(Intent(this, ChatActivity::class.java).putExtra("peer", id).putExtra("alias", alias.text.toString().takeIf { !aliasDirty && it.isNotBlank() }))
        }
        findViewById<Button>(R.id.btn_id_copy).setOnClickListener {
            val value = if (mine) ownId else contact?.contactId
            value?.let { getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText(getString(R.string.public_contact_id), it)); info.setText(R.string.id_copied) }
        }
        alias.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) { aliasDirty = true }
            override fun afterTextChanged(s: android.text.Editable?) {}
        })
        peerId.addTextChangedListener(object : android.text.TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {
                contact = null; alias.setText(""); aliasDirty = false
                info.setText(R.string.check_entered_id); controls()
            }
            override fun afterTextChanged(s: android.text.Editable?) {}
        })
    }

    override fun onResume() { super.onResume(); active = true; if (mine) showMine() else loadContact() }
    override fun onPause() { active = false; guard.stop(); prompt?.dismiss(); prompt = null; super.onPause() }
    private fun peer() = peerId.text.toString().trim()

    private fun controls() {
        val cta = contactCta(contact)
        val known = contact != null
        findViewById<Button>(R.id.btn_accept).apply { visibility = if (cta == ContactCta.Accept) View.VISIBLE else View.GONE; isEnabled = !guard.pending }
        findViewById<Button>(R.id.btn_confirm).apply { visibility = if (cta == ContactCta.VerifyChanged) View.VISIBLE else View.GONE; isEnabled = !guard.pending }
        findViewById<Button>(R.id.btn_request).apply { visibility = if (!known) View.VISIBLE else View.GONE; isEnabled = !guard.pending }
        findViewById<Button>(R.id.btn_contact_scan).isEnabled = !guard.pending && cta != ContactCta.Blocked
        findViewById<Button>(R.id.btn_block).isEnabled = !guard.pending && known && cta != ContactCta.Blocked
        for (id in listOf(R.id.btn_alias_save, R.id.btn_alias_clear, R.id.btn_contact_chat)) findViewById<Button>(id).isEnabled = !guard.pending && known
        findViewById<Button>(R.id.btn_check).isEnabled = !guard.pending
        peerId.isEnabled = !guard.pending && intent.getStringExtra("peer") == null
        alias.isEnabled = !guard.pending && known
    }

    private fun <T> act(message: String, task: DmsgFacade.() -> T, success: (T) -> Unit) {
        val stamp = guard.begin() ?: return
        info.text = message; controls()
        val fContext = applicationContext
        Core.dispatch {
            val result = runCatching { task(Core.facade(fContext)) }
            runOnUiThread {
                if (!active || !guard.finish(stamp)) return@runOnUiThread
                result.fold(success, { info.text = humanError(resources, it) }); controls()
            }
        }
    }

    private fun loadContact() {
        val id = peer()
        if (id.isEmpty()) { contact = null; info.setText(R.string.contact_input_help); controls(); return }
        act(getString(R.string.checking_contact), { Pair(get(id), summary(id)) }) { (value, summary) ->
            contact = value
            if (!aliasDirty) { alias.setText(summary?.localAlias.orEmpty()); aliasDirty = false }
            info.text = if (value == null) getString(R.string.contact_not_added) else trustLabel(resources, value)
            controls()
        }
    }

    private fun showMine() {
        act(getString(R.string.opening_qr), {
            val account = account()
            if (!account.authenticated) throw DmsgError(R.string.error_sign_in_required, ErrorKind.NotAuthenticated)
            Pair(account.contactId, renderQr(myQr()))
        }) { (id, bitmap) ->
            ownId = id
            findViewById<TextView>(R.id.my_id).text = id
            findViewById<ImageView>(R.id.qr).setImageBitmap(bitmap)
            info.setText(R.string.public_qr_disclosure)
        }
    }

    private fun renderQr(uri: String): Bitmap {
        val matrix = QRCodeWriter().encode(uri, BarcodeFormat.QR_CODE, 512, 512)
        return Bitmap.createBitmap(512, 512, Bitmap.Config.RGB_565).apply {
            for (x in 0 until 512) for (y in 0 until 512) setPixel(x, y, if (matrix.get(x, y)) 0xFF000000.toInt() else 0xFFFFFFFF.toInt())
        }
    }
}
