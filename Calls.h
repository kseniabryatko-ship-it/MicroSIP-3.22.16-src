/*
 * Copyright (C) 2011-2026 MicroSIP (http://www.microsip.org)
 *
 * This program is free software; you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation; either version 2 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program; if not, write to the Free Software
 * Foundation, Inc., 59 Temple Place, Suite 330, Boston, MA  02111-1307  USA
 */

#pragma once

#include "resource.h"
#include "global.h"
#include "BaseDialog.h"
#include "CListCtrl_SortItemsEx.h"
#include "service/CallsService.h"

class Calls :
	public CBaseDialog
{
public:
    Calls(CWnd* pParent = NULL);	// standard constructor
    ~Calls();
    enum { IDD = IDD_CALLS };

    void TabFocusSet() override {};
    bool GotoTab(int i) { return false; };
    void ProcessCommand(CString str) override {};
    
    void OnCreated();
    void Add(pj_str_t id, CString address, CString name, CString commands, int type, call_user_data* user_data);
    void DeleteAll();
    void SetName(pj_str_t id, CString name);
    void SetDuration(pj_str_t id, int sec, int total);
    void SetInfo(pj_str_t id, CString info);
    void Export(bool bFilter = false);

private:

    void CallsClear();
	void CallsLoad();

private:
    CallsService* m_callsService = nullptr;
    CListCtrl_SortItemsEx m_list;
	CImageList* imageList;
	int m_nLastDay;
    int FindListItem(int id);
	void AddListItem(std::unique_ptr<Call> call, int pos = 0);
	void MessageDlgOpen(BOOL isCall = FALSE, BOOL hasVideo = FALSE);
	void DefaultItemAction(int i);
    CString FormatTime(int time, CTime* pTimeNow = NULL);
    void ReloadTime();
    bool IsFiltered(const Call& call);
    void FilterReset();


protected:
	virtual BOOL OnInitDialog();
	afx_msg void OnTimer(UINT_PTR TimerVal);
	virtual void PostNcDestroy();
	virtual void DoDataExchange(CDataExchange* pDX);	// DDX/DDV support
	DECLARE_MESSAGE_MAP()
public:
	afx_msg void OnBnClickedOk();
	afx_msg void OnBnClickedCancel();
	afx_msg void OnFilterValueChange();
	afx_msg void OnMenuCall(); 
	afx_msg void OnMenuChat();
	afx_msg void OnMenuAdd();
	afx_msg void OnMenuCopy();
	afx_msg void OnMenuDelete(); 
	afx_msg void OnMenuExport();
	afx_msg void OnMenuImport();
	afx_msg LRESULT OnContextMenu(WPARAM wParam,LPARAM lParam);
	afx_msg void OnNMDblclkCalls(NMHDR *pNMHDR, LRESULT *pResult);
	afx_msg void OnEndtrack(NMHDR* pNMHDR, LRESULT* pResult);
#ifdef _GLOBAL_VIDEO
	afx_msg void OnMenuCallVideo(); 
#endif
	virtual BOOL PreTranslateMessage(MSG* pMsg);
};

