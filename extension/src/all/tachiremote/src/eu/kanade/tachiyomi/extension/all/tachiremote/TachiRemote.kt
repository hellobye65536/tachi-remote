package eu.kanade.tachiyomi.extension.all.tachiremote

import android.app.Application
import android.content.SharedPreferences
import android.widget.Toast
import androidx.preference.EditTextPreference
import androidx.preference.PreferenceScreen
import eu.kanade.tachiyomi.network.GET
import eu.kanade.tachiyomi.network.asObservableSuccess
import eu.kanade.tachiyomi.source.ConfigurableSource
import eu.kanade.tachiyomi.source.UnmeteredSource
import eu.kanade.tachiyomi.source.model.FilterList
import eu.kanade.tachiyomi.source.model.MangasPage
import eu.kanade.tachiyomi.source.model.Page
import eu.kanade.tachiyomi.source.model.SChapter
import eu.kanade.tachiyomi.source.model.SManga
import eu.kanade.tachiyomi.source.online.HttpSource
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.decodeFromStream
import okhttp3.Dns
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import rx.Observable
import uy.kohesive.injekt.Injekt
import uy.kohesive.injekt.api.get

typealias LibraryDto = List<LibraryEntryDto>

@Serializable
data class LibraryEntryDto(
    val id: String,
    val title: String,
)

@Serializable
data class MangaDto(
    val title: String,
    val status: Int = 0,
    val description: String = "",
    val authors: String = "",
    val artists: String = "",
    val tags: String = "",
    val chapters: List<ChapterDto> = emptyList(),
)

@Serializable
data class ChapterDto(
    val title: String,
    val date: Long = 0,
    val pages: Int,
)

open class TachiRemote(suffix: String = "") : ConfigurableSource, UnmeteredSource, HttpSource() {
    override val name = "TachiRemote$suffix"
    override val supportsLatest = false

    override val lang = "all"

    override val baseUrl by lazy { preferences.getString(ADDRESS_KEY, "")!! }

    override val client: OkHttpClient = network.client.newBuilder().dns(Dns.SYSTEM).build()

    private val preferences: SharedPreferences by lazy {
        Injekt.get<Application>().getSharedPreferences("source_$id", 0x0000)
    }

    private val json = Json {
        ignoreUnknownKeys = true
    }

    override fun setupPreferenceScreen(screen: PreferenceScreen) {
        screen.addPreference(
            EditTextPreference(screen.context).apply {
                key = ADDRESS_KEY
                title = ADDRESS_TITLE
                summary = baseUrl
                dialogTitle = ADDRESS_TITLE

                setOnPreferenceChangeListener { _, newValue ->
                    val res = preferences.edit().putString(ADDRESS_KEY, newValue as String).commit()
                    Toast.makeText(
                        screen.context,
                        "Restart Tachiyomi to apply new setting.",
                        Toast.LENGTH_LONG,
                    ).show()
                    res
                }
            },
        )
    }

    override fun popularMangaRequest(page: Int): Request = GET(baseUrl, headers)
    override fun popularMangaParse(response: Response): MangasPage {
        val responseBody =
            response.body ?: throw IllegalStateException("Response code ${response.code}")

        val libDto = responseBody.use {
            json.decodeFromStream<LibraryDto>(it.byteStream())
        }.toMangas()

        return MangasPage(libDto, false)
    }

    override fun latestUpdatesRequest(page: Int) = throw UnsupportedOperationException("Not used")
    override fun latestUpdatesParse(response: Response) =
        throw UnsupportedOperationException("Not used")

    override fun fetchSearchManga(
        page: Int,
        query: String,
        filters: FilterList,
    ): Observable<MangasPage> = fetchPopularManga(page)
    // Observable.just(MangasPage(emptyList(), false))

    override fun searchMangaRequest(page: Int, query: String, filters: FilterList) =
        throw UnsupportedOperationException("Not used")

    override fun searchMangaParse(response: Response) =
        throw UnsupportedOperationException("Not used")

    override fun fetchMangaDetails(manga: SManga): Observable<SManga> =
        fetchMangaDto(manga).map { it.toManga(manga) }

    override fun mangaDetailsParse(response: Response) =
        throw UnsupportedOperationException("Not used")

    override fun fetchChapterList(manga: SManga): Observable<List<SChapter>> =
        fetchMangaDto(manga).map { it.toChapters(it.toManga(manga)) }

    override fun chapterListParse(response: Response) =
        throw UnsupportedOperationException("Not used")

    override fun fetchPageList(chapter: SChapter): Observable<List<Page>> =
        Observable.just(chapter.toPageList())

    override fun pageListParse(response: Response) = throw UnsupportedOperationException("Not used")
    override fun imageUrlParse(response: Response) = throw UnsupportedOperationException("Not used")

    private fun fetchMangaDto(manga: SManga): Observable<MangaDto> =
        client.newCall(mangaDetailsRequest(manga)).asObservableSuccess().map { response ->
            val responseBody =
                response.body ?: throw IllegalStateException("Response code ${response.code}")

            responseBody.use { json.decodeFromStream<MangaDto>(it.byteStream()) }
        }

    private fun LibraryDto.toMangas(): List<SManga> = this.map { it.toManga() }

    private fun LibraryEntryDto.toManga(): SManga = SManga.create().apply {
        url = "/${this@toManga.id}"
        title = this@toManga.title
        thumbnail_url = "$baseUrl$url/cover"
        initialized = false
    }

    private fun MangaDto.toManga(manga: SManga): SManga = manga.apply {
        title = this@toManga.title
        thumbnail_url = "$baseUrl$url/cover"
        author = this@toManga.authors
        artist = this@toManga.artists
        description = this@toManga.description
        genre = this@toManga.tags
        status = this@toManga.status
        initialized = true
    }

    private fun MangaDto.toChapters(manga: SManga): List<SChapter> =
        chapters.mapIndexed { index, it ->
            it.toChapter(manga, index)
        }.asReversed()

    private fun ChapterDto.toChapter(manga: SManga, index: Int): SChapter = SChapter.create().apply {
        // hack: store page count inline in url
        url = "${manga.url}/$index/$pages"
        name = this@toChapter.title
        date_upload = this@toChapter.date
        chapter_number = (index + 1).toFloat()
    }

    private fun SChapter.toPageList(): List<Page> {
        val url = this.url.substringBeforeLast('/')
        val pages = this.url.substringAfterLast('/').toInt()

        return (0 until pages).map {
            Page(it, imageUrl = "$baseUrl$url/$it")
        }
    }

    companion object {
        private const val ADDRESS_KEY = "address"
        private const val ADDRESS_TITLE = "Address"
    }
}
