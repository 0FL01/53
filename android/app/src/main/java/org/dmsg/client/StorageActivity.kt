package org.dmsg.client

import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.appcompat.app.AlertDialog

/** Same-install snapshots and cache are distinct; no history deletion or key export CTA. */
class StorageActivity : DmsgActivity() {
    private val guard = UiGuard()
    private var active = false
    private var prompt: AlertDialog? = null
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); setContentView(R.layout.activity_storage)
        NativeUi.back(this, "Хранилище")
        findViewById<Button>(R.id.btn_migrate).setOnClickListener {
            work("Сохраняем защищённую копию…", { SecureStore.seal(applicationContext) }) { "Локальная копия обновлена. Восстановление возможно только в этой установке с исходным Keystore-ключом." }
        }
        findViewById<Button>(R.id.btn_unseal).setOnClickListener {
            prompt = AlertDialog.Builder(this).setTitle("Восстановить локальную копию?")
                .setMessage("Нужен исходный Keystore-ключ этой установки. Существующая рабочая база не будет перезаписана. Пароль аккаунта не восстанавливает удалённые ключи и историю.")
                .setNegativeButton("Отмена", null).setPositiveButton("Восстановить") { _, _ ->
                    work("Проверяем и восстанавливаем…", { SecureStore.unseal(applicationContext) }) { "Копия восстановлена, identity проверена ядром." }
                }.show()
        }
        findViewById<Button>(R.id.btn_wipe_cache).setOnClickListener {
            work("Очищаем временные файлы…", { cacheDir.deleteRecursively() }) {
                if (it) "Временный кэш очищен. История, ключи и локальная копия сохранены." else "Часть временного кэша не удалось удалить. История и ключи сохранены."
            }
        }
    }
    override fun onResume() { super.onResume(); active = true; showPlan() }
    override fun onPause() { active = false; guard.stop(); prompt?.dismiss(); prompt = null; super.onPause() }
    private fun controls() {
        for (id in listOf(R.id.btn_migrate, R.id.btn_unseal, R.id.btn_wipe_cache)) findViewById<Button>(id).isEnabled = !guard.pending
    }
    private fun <T> work(message: String, task: () -> T, success: (T) -> String) {
        val stamp = guard.begin() ?: return
        findViewById<TextView>(R.id.info).text = message; controls()
        Core.dispatch {
            val result = runCatching(task)
            runOnUiThread {
                if (!active || !guard.finish(stamp)) return@runOnUiThread
                findViewById<TextView>(R.id.info).text = result.fold(success, ::humanError)
                controls(); showPlan()
            }
        }
    }
    private fun showPlan() {
        val stamp = guard.begin() ?: return
        controls()
        Core.dispatch {
            val result = runCatching { Triple(SecureStore.plan(applicationContext), Core.dbFile(applicationContext).exists(), SecureStore.sealedDb(applicationContext).exists()) }
            runOnUiThread {
            if (!active || !guard.finish(stamp)) return@runOnUiThread
            result.fold({ (plan, live, copy) ->
            findViewById<TextView>(R.id.plan).text = when (plan) {
                "ready" -> "Keystore-обёртка ключа сохранена. Ключ проверяется ядром при открытии."
                "reinstall_loss" -> "Keystore-ключ утрачен. Локальную копию открыть нельзя; новая identity не создаётся автоматически."
                "migrate" -> "Найдена локальная база без обёрнутого ключа. Старые схемы отвергаются без переноса и без автоматического сброса."
                else -> "Новая установка. Защищённое хранилище создаётся при первом открытии ядра."
            } + "\nРабочая база: ${if (live) "есть" else "нет"}\nЛокальная копия: ${if (copy) "есть" else "нет"}"
            }, { findViewById<TextView>(R.id.plan).text = humanError(it) })
            controls()
            }
        }
    }
}
